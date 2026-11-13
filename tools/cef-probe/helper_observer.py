"""Targeted, read-only macOS helper evidence; never scans, signals or deletes.

Usage: construct HelperObserver(root.pid, app, canary, framework_file) before
reading native stderr, call observe(line) for each complete line (or batch of
lines), then JSON-serialize finish(). Feed records promptly: buffered logs read
after helper exit cannot establish ownership or post-initialization policy.
An unterminated but otherwise complete line is accepted; split records are not.
Policy queries are point-in-time policy evidence, not actual helper file I/O or
a general proof of confinement. The caller owns the unique bundle and canary.
"""

import ctypes
import errno
import math
import os
import re
import select
import stat
import sys
import time
from dataclasses import dataclass
from pathlib import Path

MAX_HELPERS = 64
MAX_LINE = 1024
PID_MAX = 2**31 - 1
ROLES = ("renderer", "gpu-process", "utility")
TOKEN = r"[A-Za-z0-9_.-]{1,128}"
START = re.compile(
    rf"CEF_HELPER_START pid=([1-9][0-9]{{0,9}}) ppid=([1-9][0-9]{{0,9}}) "
    rf"role=(renderer|gpu-process|utility) subtype=({TOKEN}) "
    rf"sandbox=({TOKEN}) seatbelt=([01])"
)
READY = re.compile(r"CEF_HELPER_SANDBOX_READY pid=([1-9][0-9]{0,9})")
FRAMEWORK = "Chromium Embedded Framework.framework"
HELPER_NAMES = tuple(
    "felinaut-cef-probe " + suffix
    for suffix in (
        "Helper",
        "Helper (GPU)",
        "Helper (Renderer)",
        "Helper (Plugin)",
        "Helper (Alerts)",
    )
)


class NativeError(RuntimeError):
    pass


# macOS SDK sys/proc_info.h: PROC_PIDTBSDINFO=3, struct proc_bsdinfo;
# sys/param.h: MAXCOMLEN=16; uid_t/gid_t are uint32_t. No packed layout.
class ProcBsdInfo(ctypes.Structure):
    _fields_ = (
        [
            (name, ctypes.c_uint32)
            for name in (
                "pbi_flags",
                "pbi_status",
                "pbi_xstatus",
                "pbi_pid",
                "pbi_ppid",
                "pbi_uid",
                "pbi_gid",
                "pbi_ruid",
                "pbi_rgid",
                "pbi_svuid",
                "pbi_svgid",
                "rfu_1",
            )
        ]
        + [
            ("pbi_comm", ctypes.c_char * 16),
            ("pbi_name", ctypes.c_char * 32),
        ]
        + [
            (name, ctypes.c_uint32)
            for name in (
                "pbi_nfiles",
                "pbi_pgid",
                "pbi_pjobc",
                "e_tdev",
                "e_tpgid",
            )
        ]
        + [
            ("pbi_nice", ctypes.c_int32),
            ("pbi_start_tvsec", ctypes.c_uint64),
            ("pbi_start_tvusec", ctypes.c_uint64),
        ]
    )


@dataclass(frozen=True)
class ProcessInfo:
    pid: int
    ppid: int
    uid: int
    ruid: int
    svuid: int
    start: tuple
    status: int = 2


class NativeAPI:
    """Small injectable seam. None from info means ESRCH, never permission failure."""

    def __init__(self):
        if sys.platform != "darwin":
            raise NativeError("macOS libproc/kqueue APIs unavailable on this platform")
        self.queue = None
        self.exits = set()
        try:
            self.proc = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
            self.proc.proc_pidinfo.argtypes = [
                ctypes.c_int,
                ctypes.c_int,
                ctypes.c_uint64,
                ctypes.c_void_p,
                ctypes.c_int,
            ]
            self.proc.proc_pidinfo.restype = ctypes.c_int
            self.proc.proc_pidpath.argtypes = [
                ctypes.c_int,
                ctypes.c_void_p,
                ctypes.c_uint32,
            ]
            self.proc.proc_pidpath.restype = ctypes.c_int
            if ctypes.sizeof(ProcBsdInfo) != 136:
                raise NativeError("unexpected proc_bsdinfo ABI size")
            self.queue = select.kqueue()
        except (OSError, AttributeError) as error:
            raise NativeError(f"libproc/kqueue unavailable: {error}") from error
        self.policy_error = None
        try:
            self.system = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
            self.check = self.system.sandbox_check
            # sandbox_check is variadic: only the THREE fixed parameters belong
            # in argtypes (essential on Apple arm64). Paths are explicit varargs.
            self.check.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int]
            self.check.restype = ctypes.c_int
            # This is exported DATA, not a function or a guessed flag constant.
            self.no_report = ctypes.c_int.in_dll(
                self.system, "SANDBOX_CHECK_NO_REPORT"
            ).value
            if self.no_report <= 1 or self.no_report & 1:
                raise NativeError("invalid SANDBOX_CHECK_NO_REPORT data")
        except (OSError, AttributeError, ValueError, NativeError) as error:
            self.policy_error = f"sandbox_check API unavailable: {error}"

    def info(self, pid):
        data = ProcBsdInfo()
        ctypes.set_errno(0)
        count = self.proc.proc_pidinfo(
            pid, 3, 0, ctypes.byref(data), ctypes.sizeof(data)
        )
        if count != ctypes.sizeof(data):
            code = ctypes.get_errno()
            if count <= 0 and code == errno.ESRCH:
                return None
            raise NativeError(
                f"proc_pidinfo failed/short read (errno={code}, size={count})"
            )
        if (
            data.pbi_pid != pid
            or not data.pbi_start_tvsec
            or data.pbi_start_tvusec >= 1000000
        ):
            raise NativeError("proc_pidinfo returned invalid identity")
        return ProcessInfo(
            data.pbi_pid,
            data.pbi_ppid,
            data.pbi_uid,
            data.pbi_ruid,
            data.pbi_svuid,
            (data.pbi_start_tvsec, data.pbi_start_tvusec),
            data.pbi_status,
        )

    def path(self, pid):
        # SDK PROC_PIDPATHINFO_MAXSIZE = 4 * MAXPATHLEN = 4096.
        buffer = ctypes.create_string_buffer(4096)
        ctypes.set_errno(0)
        count = self.proc.proc_pidpath(pid, buffer, len(buffer))
        if count <= 0 or count >= len(buffer) or not buffer.value:
            raise NativeError(f"proc_pidpath failed (errno={ctypes.get_errno()})")
        return os.fsdecode(buffer.value)

    def register(self, pid):
        event = select.kevent(
            pid,
            filter=select.KQ_FILTER_PROC,
            flags=select.KQ_EV_ADD | select.KQ_EV_ENABLE | select.KQ_EV_ONESHOT,
            fflags=select.KQ_NOTE_EXIT,
        )
        # A registration call may also drain exits for previously registered PIDs.
        self.exits.update(self._events(self.queue.control([event], MAX_HELPERS, 0)))

    @staticmethod
    def _events(events):
        exits = set()
        for event in events:
            if event.flags & select.KQ_EV_ERROR:
                raise NativeError(f"kqueue EV_ERROR (errno={event.data})")
            if (
                event.filter == select.KQ_FILTER_PROC
                and event.fflags & select.KQ_NOTE_EXIT
            ):
                exits.add(event.ident)
        return exits

    def poll(self, timeout):
        self.exits.update(self._events(self.queue.control(None, MAX_HELPERS, timeout)))
        events, self.exits = self.exits, set()
        return events

    def query(self, pid, path=None):
        if self.policy_error:
            raise NativeError(self.policy_error)
        ctypes.set_errno(0)
        if path is None:
            # Presence query: NULL operation, filter NONE=0 (not NO_REPORT).
            result = self.check(pid, None, 0)
        else:
            result = self.check(
                pid,
                b"file-read-data",
                1 | self.no_report,
                ctypes.c_char_p(os.fsencode(path)),
            )
        code = ctypes.get_errno()
        if result not in (0, 1) or code:
            raise NativeError(f"sandbox_check failed (result={result}, errno={code})")
        return result

    def close(self):
        if self.queue is not None:
            self.queue.close()
            self.queue = None


def _open_canary(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise NativeError("canary is not a regular file")
    finally:
        os.close(fd)


class HelperObserver:
    """observe accepts complete lines, not cumulative logs; finish is idempotent."""

    def __init__(self, root_pid, app_path, canary_path, allowed_bundle_file):
        self.root_pid = root_pid
        self.helpers = {}
        self.launch_count = 0
        self.record_counts = {"start": 0, "ready": 0, "launch": 0}
        self.gaps = []
        self.api = None
        self.report = None
        self.control = {"canary_open": False, "allowed_bundle_file": False}
        try:
            if type(root_pid) is not int or not 0 < root_pid <= PID_MAX:
                raise NativeError("root_pid must be a positive pid_t")
            paths = [Path(p) for p in (app_path, canary_path, allowed_bundle_file)]
            if not all(p.is_absolute() and ".." not in p.parts for p in paths):
                raise NativeError("paths must be absolute without parent traversal")
            self.app, self.canary, self.allowed = (p.resolve() for p in paths)
            if self.app.suffix != ".app":
                raise NativeError("app_path must identify the unique probe .app")
            framework = self.app / "Contents/Frameworks" / FRAMEWORK
            if not self.allowed.is_relative_to(framework) or not self.allowed.is_file():
                raise NativeError(
                    "allowed_bundle_file must be an existing file in the app's framework"
                )
            if self.canary.is_relative_to(self.app) or self.canary == self.allowed:
                raise NativeError("canary must be separate from the allowed app bundle")
            self.helper_paths = {
                str(
                    self.app
                    / "Contents/Frameworks"
                    / (name + ".app")
                    / "Contents/MacOS"
                    / name
                )
                for name in HELPER_NAMES
            }
            self.control["allowed_bundle_file"] = True
            _open_canary(self.canary)
            self.control["canary_open"] = True
            self.api = NativeAPI()
        except (OSError, ValueError, TypeError, NativeError) as error:
            self._gap(f"observer initialization failed: {error}")

    def _gap(self, message, helper=None):
        target = self.gaps if helper is None else helper["gaps"]
        if message not in target and len(target) < MAX_HELPERS:
            target.append(message)

    def observe(self, text):
        if self.report is not None:
            raise RuntimeError("observe after finish")
        if not isinstance(text, str):
            self._gap("observe requires decoded text lines")
            return
        # Iterate without splitlines allocating a list proportional to log size.
        for match in re.finditer(r"[^\n]+", text):
            if not text.startswith("CEF_HELPER_", match.start()):
                continue
            if match.end() - match.start() > MAX_LINE:
                self._gap("helper record exceeds line limit")
                continue
            line = match.group().removesuffix("\r")
            start, ready = START.fullmatch(line), READY.fullmatch(line)
            if start:
                kind = "start"
            elif ready:
                kind = "ready"
            elif line == "CEF_HELPER_LAUNCH":
                kind = "launch"
            else:
                self._gap("malformed or incomplete helper record")
                continue
            self.record_counts[kind] = min(
                self.record_counts[kind] + 1, MAX_HELPERS + 1
            )
            if self.record_counts[kind] > MAX_HELPERS:
                self._gap(f"{kind} record limit exceeded ({MAX_HELPERS})")
                continue
            if kind == "launch":
                self.launch_count += 1
                continue
            pid = int((start or ready)[1])
            if pid > PID_MAX:
                self._gap("helper PID exceeds pid_t range")
                continue
            if kind == "start":
                self._start(pid, start)
            else:
                self._ready(pid)

    def _owned(self, info):
        return (
            info is not None
            and info.ppid == self.root_pid
            and info.uid == info.ruid == info.svuid == os.getuid()
            and info.status != 5  # SDK sys/proc.h SZOMB; not a live registration.
        )

    def _start(self, pid, match):
        if pid in self.helpers:
            self._gap("duplicate START record", self.helpers[pid])
            return
        helper = {
            "pid": pid,
            "ppid": int(match[2]),
            "role": match[3],
            "subtype": match[4],
            "sandbox": match[5],
            "seatbelt": int(match[6]),
            "identity": None,
            "executable": None,
            "registered": False,
            "ready": False,
            "queries": {},
            "policy_verified": False,
            "exit_evidence": "unknown",
            "gaps": [],
        }
        self.helpers[pid] = helper
        if helper["ppid"] != self.root_pid:
            self._gap("announced parent is not owned root", helper)
            return
        if self.api is None:
            self._gap("native identity/exit API unavailable", helper)
            return
        try:
            before = self.api.info(pid)
            if not self._owned(before):
                raise NativeError(
                    "helper missing, not live, or UID/parent is foreign at START"
                )
            path = self.api.path(pid)
            if path not in self.helper_paths:
                raise NativeError(
                    "executable is not an exact helper variant in this app"
                )
            self.api.register(pid)
            after = self.api.info(pid)
            if not self._owned(after) or after.start != before.start:
                raise NativeError(
                    "identity lost/changed before positive live registration"
                )
            helper.update(
                identity=list(before.start),
                executable=path,
                uid=before.uid,
                registered=True,
            )
        except (OSError, NativeError) as error:
            self._gap(f"START verification failed: {error}", helper)

    def _ready(self, pid):
        helper = self.helpers.get(pid)
        if helper is None:
            self._gap("READY without preceding START")
            return
        if helper["ready"]:
            self._gap("duplicate READY record", helper)
            return
        helper["ready"] = True
        if not helper["registered"]:
            self._gap("READY without verified live registration", helper)
            return
        try:
            before = self.api.info(pid)
            if not self._owned(before) or list(before.start) != helper["identity"]:
                raise NativeError("identity missing/foreign/reused at READY")
            if self.api.path(pid) != helper["executable"]:
                raise NativeError("helper executable changed at READY")
            for name, path, expected in (
                ("presence", None, 1),
                ("allowed_bundle", self.allowed, 0),
                ("canary", self.canary, 1),
            ):
                query = {
                    "operation": None if path is None else "file-read-data",
                    "filter": "NONE" if path is None else "PATH|NO_REPORT",
                    "path": None if path is None else str(path),
                    "expected": expected,
                    "result": None,
                    "error": None,
                }
                helper["queries"][name] = query
                try:
                    result = self.api.query(pid, path)
                    if type(result) is not int or result not in (0, 1):
                        raise NativeError("invalid policy result (not denial evidence)")
                    query["result"] = result
                    if result != expected:
                        self._gap(f"{name} policy control mismatch", helper)
                except (OSError, NativeError) as error:
                    query["error"] = str(error)
                    self._gap(f"{name} policy query failed", helper)
            after = self.api.info(pid)
            if not self._owned(after) or after.start != before.start:
                raise NativeError("identity lost/changed during READY policy queries")
            if self.api.path(pid) != helper["executable"]:
                raise NativeError("helper executable changed during policy queries")
            helper["policy_verified"] = all(
                q["error"] is None and q["result"] == q["expected"]
                for q in helper["queries"].values()
            )
        except (OSError, NativeError) as error:
            self._gap(f"READY verification failed: {error}", helper)

    def finish(self, deadline_seconds=3):
        if self.report is not None:
            return self.report
        try:
            valid_deadline = (
                type(deadline_seconds) in (int, float)
                and math.isfinite(deadline_seconds)
                and deadline_seconds >= 0
            )
        except OverflowError:
            valid_deadline = False
        if not valid_deadline:
            self._gap("exit deadline must be finite and nonnegative")
            deadline_seconds = 0
        deadline = time.monotonic() + deadline_seconds
        pending = {pid for pid, h in self.helpers.items() if h["registered"]}
        try:
            while pending:
                try:
                    events = self.api.poll(0)
                except (OSError, NativeError) as error:
                    self._gap(f"kqueue polling failed: {error}")
                    events = set()
                for pid in list(pending):
                    helper = self.helpers[pid]
                    if pid in events:
                        helper["exit_evidence"] = "kqueue_NOTE_EXIT"
                    else:
                        try:
                            current = self.api.info(pid)
                            if current is None:
                                helper["exit_evidence"] = "registered_identity_absent"
                            elif list(current.start) != helper["identity"]:
                                helper["exit_evidence"] = "registered_identity_replaced"
                        except (OSError, NativeError) as error:
                            self._gap(f"exit identity query failed: {error}", helper)
                    if helper["exit_evidence"] != "unknown":
                        pending.remove(pid)
                remaining = deadline - time.monotonic()
                if not pending or remaining <= 0:
                    break
                # No process signals, waits/reaping, or global PID discovery.
                time.sleep(min(0.02, remaining))
        finally:
            if self.api is not None:
                try:
                    self.api.close()
                except OSError as error:
                    self._gap(f"kqueue close failed: {error}")
        if not self.helpers:
            self._gap("no helper START records observed")
        if self.launch_count != len(self.helpers):
            self._gap("host launch count does not match distinct helper START count")
        for helper in self.helpers.values():
            if not helper["ready"]:
                self._gap(
                    "missing SANDBOX_READY record; no post-init policy evidence", helper
                )
            if helper["registered"] and helper["exit_evidence"] == "unknown":
                self._gap(
                    "registered helper survives deadline or exit identity is unreadable",
                    helper,
                )
        roles = {}
        for role in ROLES:
            helpers = [h for h in self.helpers.values() if h["role"] == role]
            roles[role] = {
                "count": len(helpers),
                "policy_verified": bool(helpers)
                and all(h["policy_verified"] for h in helpers),
                "exits_verified": bool(helpers)
                and all(h["exit_evidence"] != "unknown" for h in helpers),
                "gaps": [] if helpers else ["role not observed; no evidence"],
            }
        complete = (
            bool(self.helpers)
            and not self.gaps
            and all(not h["gaps"] for h in self.helpers.values())
        )
        self.report = {
            "ok": complete and all(h["policy_verified"] for h in self.helpers.values()),
            "root_pid": self.root_pid if type(self.root_pid) is int else None,
            "host_launch_count": self.launch_count,
            "distinct_starts": len(self.helpers),
            "record_counts": self.record_counts,
            "controls": self.control,
            "roles": roles,
            "helpers": list(self.helpers.values()),
            "gaps": self.gaps,
            "evidence_scope": "announced owned helpers only; point-in-time sandbox policy queries, not helper I/O",
            "cleanup": "none; observer never signals processes or deletes files",
        }
        return self.report
