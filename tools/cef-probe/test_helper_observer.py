"""Offline observer tests: fake native snapshots, policy calls and kqueue events.

Run: python3 -B -m unittest discover -s tools/cef-probe -p test_helper_observer.py
No native launches, process enumeration, temporary files or cleanup deletion.
"""

import ctypes
import errno
import json
import os
import unittest
from dataclasses import replace
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

import helper_observer as observer

ROOT = 41000
APP = Path("/__cef_observer__/unique.app")
CANARY = Path("/__cef_observer__/generated-canary")
ALLOWED = APP / "Contents/Frameworks" / observer.FRAMEWORK / "Resources/icudtl.dat"


def helper_path(index=0, app=APP):
    name = observer.HELPER_NAMES[index]
    return str(app / "Contents/Frameworks" / (name + ".app") / "Contents/MacOS" / name)


def start(pid=41001, role="renderer", **fields):
    values = dict(ppid=ROOT, subtype="none", sandbox="renderer", seatbelt=1)
    values.update(fields)
    return (
        f"CEF_HELPER_START pid={pid} ppid={values['ppid']} role={role} "
        f"subtype={values['subtype']} sandbox={values['sandbox']} seatbelt={values['seatbelt']}"
    )


def ready(pid=41001):
    return f"CEF_HELPER_SANDBOX_READY pid={pid}"


class FakeNative:
    def __init__(self):
        self.processes = {}
        self.paths = {}
        self.info_calls = []
        self.path_calls = []
        self.registered = []
        self.query_calls = []
        self.events = set()
        self.results = {None: 1, str(ALLOWED): 0, str(CANARY): 1}
        self.register_error = None
        self.poll_error = None
        self.on_register = lambda pid: None
        self.on_query = lambda pid: None
        self.closed = False

    def add(self, pid=41001, variant=0, **changes):
        self.processes[pid] = replace(
            observer.ProcessInfo(
                pid,
                ROOT,
                os.getuid(),
                os.getuid(),
                os.getuid(),
                (100, pid),
            ),
            **changes,
        )
        self.paths[pid] = helper_path(variant)

    def info(self, pid):
        self.info_calls.append(pid)
        result = self.processes.get(pid)
        if isinstance(result, Exception):
            raise result
        return result

    def path(self, pid):
        self.path_calls.append(pid)
        result = self.paths[pid]
        if isinstance(result, Exception):
            raise result
        return result

    def register(self, pid):
        if self.register_error:
            raise self.register_error
        self.registered.append(pid)
        self.on_register(pid)

    def poll(self, timeout):
        if self.poll_error:
            raise self.poll_error
        result, self.events = self.events, set()
        return result

    def query(self, pid, path):
        self.query_calls.append((pid, path))
        self.on_query(pid)
        result = self.results[None if path is None else str(path)]
        if isinstance(result, Exception):
            raise result
        return result

    def close(self):
        self.closed = True


class ObserverTests(unittest.TestCase):
    def setUp(self):
        self.native = FakeNative()
        self.native.add()
        for patcher in (
            patch.object(observer, "NativeAPI", return_value=self.native),
            patch.object(observer, "_open_canary"),
            patch.object(Path, "is_file", return_value=True),
        ):
            patcher.start()
            self.addCleanup(patcher.stop)
        self.subject = observer.HelperObserver(ROOT, APP, CANARY, ALLOWED)

    def record(self, pid=41001, role="renderer", **fields):
        self.subject.observe("CEF_HELPER_LAUNCH\n" + start(pid, role, **fields))
        self.subject.observe(ready(pid))

    def finish(self):
        report = self.subject.finish(0)
        json.dumps(report, allow_nan=False)
        self.assertTrue(self.native.closed)
        return report

    def test_success_all_roles_queries_exit_and_json(self):
        for i, role in enumerate(observer.ROLES):
            pid = 41001 + i
            self.native.add(pid, variant=i)
            self.record(
                pid, role, subtype="network.mojom.NetworkService" if i == 2 else "none"
            )
        self.assertEqual(len(self.native.query_calls), 9)
        self.native.events = {41001, 41002, 41003}
        report = self.finish()
        self.assertTrue(report["ok"])
        self.assertEqual(report["host_launch_count"], 3)
        for role in report["roles"].values():
            self.assertTrue(role["policy_verified"])
            self.assertTrue(role["exits_verified"])
        for helper in report["helpers"]:
            self.assertEqual(helper["exit_evidence"], "kqueue_NOTE_EXIT")
            self.assertEqual(helper["queries"]["canary"]["result"], 1)
            self.assertEqual(helper["queries"]["allowed_bundle"]["result"], 0)
        self.assertEqual(set(self.native.info_calls), {41001, 41002, 41003})

    def test_no_policy_queries_before_ready(self):
        self.subject.observe("CEF_HELPER_LAUNCH\n" + start())
        self.assertEqual(self.native.query_calls, [])
        self.native.events.add(41001)
        report = self.finish()
        self.assertFalse(report["ok"])
        self.assertIn("post-init", " ".join(report["helpers"][0]["gaps"]))
        self.assertFalse(report["roles"]["renderer"]["policy_verified"])

    def test_missing_pid_at_start_is_not_exit_success(self):
        self.native.processes.clear()
        self.record()
        report = self.finish()
        self.assertFalse(report["ok"])
        self.assertFalse(report["helpers"][0]["registered"])
        self.assertEqual(report["helpers"][0]["exit_evidence"], "unknown")
        self.assertEqual(self.native.registered, [])
        self.assertEqual(self.native.query_calls, [])

    def test_unreadable_pid_or_path_is_not_owned(self):
        for stage in ("info", "path"):
            with self.subTest(stage=stage):
                self.setUp_subject()
                if stage == "info":
                    self.native.processes[41001] = PermissionError("denied")
                else:
                    self.native.paths[41001] = PermissionError("denied")
                self.record()
                self.assertFalse(self.finish()["ok"])
                self.assertEqual(self.native.registered, [])

    def setUp_subject(self):
        self.native.__init__()
        self.native.add()
        self.subject = observer.HelperObserver(ROOT, APP, CANARY, ALLOWED)

    def test_foreign_parent_uid_saved_uid_and_zombie_rejected(self):
        for changes in (
            {"ppid": ROOT + 1},
            {"uid": os.getuid() + 1},
            {"ruid": os.getuid() + 1},
            {"svuid": os.getuid() + 1},
            {"status": 5},
        ):
            with self.subTest(changes=changes):
                self.setUp_subject()
                self.native.add(**changes)
                self.record()
                self.assertFalse(self.finish()["ok"])
                self.assertEqual(self.native.registered, [])

    def test_announced_foreign_parent_not_inspected(self):
        self.record(ppid=ROOT + 1)
        self.assertFalse(self.finish()["ok"])
        self.assertEqual(self.native.info_calls, [])

    def test_only_exact_variant_paths_in_unique_app(self):
        for path in (
            helper_path(app=Path("/__cef_observer__/other.app")),
            helper_path() + ".foreign",
            helper_path() + "/../Helper",
            str(APP / "Contents/MacOS/felinaut-cef-probe"),
        ):
            with self.subTest(path=path):
                self.setUp_subject()
                self.native.paths[41001] = path
                self.record()
                self.assertFalse(self.finish()["ok"])
                self.assertEqual(self.native.registered, [])
        for variant in range(5):
            with self.subTest(variant=variant):
                self.setUp_subject()
                self.native.add(variant=variant)
                self.record()
                self.native.events.add(41001)
                self.assertTrue(self.finish()["ok"])

    def test_exit_reuse_or_parent_change_during_registration_is_unknown(self):
        def replace_pid(pid):
            self.native.processes[pid] = replace(
                self.native.processes[pid], start=(101, 0)
            )

        def reparent(pid):
            self.native.processes[pid] = replace(self.native.processes[pid], ppid=1)

        for action in (
            lambda pid: self.native.processes.pop(pid),
            replace_pid,
            reparent,
        ):
            with self.subTest(action=action):
                self.setUp_subject()
                self.native.on_register = action
                self.record()
                self.native.events.add(41001)
                helper = self.finish()["helpers"][0]
                self.assertFalse(helper["registered"])
                self.assertEqual(helper["exit_evidence"], "unknown")

    def test_failed_registration_is_unknown_even_if_pid_later_disappears(self):
        self.native.register_error = ProcessLookupError("already gone")
        self.record()
        self.native.processes.clear()
        self.assertEqual(self.finish()["helpers"][0]["exit_evidence"], "unknown")

    def test_missing_reused_or_exec_changed_at_ready_never_queried(self):
        for change in ("gone", "reuse", "exec"):
            with self.subTest(change=change):
                self.setUp_subject()
                self.subject.observe("CEF_HELPER_LAUNCH\n" + start())
                if change == "gone":
                    self.native.processes.clear()
                elif change == "reuse":
                    self.native.add(start=(200, 0))
                else:
                    self.native.paths[41001] = "/foreign"
                self.subject.observe(ready())
                self.assertFalse(self.finish()["ok"])
                self.assertEqual(self.native.query_calls, [])

    def test_reuse_or_exit_during_queries_invalidates_policy(self):
        for gone in (True, False):
            with self.subTest(gone=gone):
                self.setUp_subject()

                def change(pid):
                    if gone:
                        self.native.processes.pop(pid, None)
                    else:
                        self.native.add(start=(200, 0))

                self.native.on_query = change
                self.record()
                report = self.finish()
                self.assertFalse(report["ok"])
                self.assertFalse(report["helpers"][0]["policy_verified"])

    def test_failed_queries_never_count_as_denial(self):
        for key in (None, str(ALLOWED), str(CANARY)):
            for result in (-1, 2, None, True, PermissionError("unavailable")):
                with self.subTest(key=key, result=result):
                    self.setUp_subject()
                    self.native.results[key] = result
                    self.record()
                    self.native.events.add(41001)
                    report = self.finish()
                    self.assertFalse(report["ok"])
                    self.assertFalse(report["helpers"][0]["policy_verified"])

    def test_query_control_mismatches(self):
        for key, result in ((None, 0), (str(ALLOWED), 1), (str(CANARY), 0)):
            with self.subTest(key=key):
                self.setUp_subject()
                self.native.results[key] = result
                self.record()
                self.native.events.add(41001)
                self.assertFalse(self.finish()["ok"])

    def test_registered_identity_absent_or_replaced_proves_original_exit(self):
        for reuse in (False, True):
            with self.subTest(reuse=reuse):
                self.setUp_subject()
                self.record()
                if reuse:
                    self.native.add(start=(200, 0), ppid=1, uid=os.getuid() + 1)
                    self.native.paths[41001] = "/foreign"
                else:
                    self.native.processes.clear()
                report = self.finish()
                self.assertTrue(report["ok"])
                self.assertEqual(
                    report["helpers"][0]["exit_evidence"],
                    "registered_identity_replaced"
                    if reuse
                    else "registered_identity_absent",
                )

    def test_reparented_same_identity_survives_and_is_not_signalled(self):
        self.record()
        self.native.add(ppid=1)
        report = self.finish()
        self.assertFalse(report["ok"])
        self.assertEqual(report["helpers"][0]["exit_evidence"], "unknown")

    def test_unreadable_finish_is_not_exit(self):
        self.record()
        self.native.processes[41001] = PermissionError("unreadable")
        report = self.finish()
        self.assertFalse(report["ok"])
        self.assertEqual(report["helpers"][0]["exit_evidence"], "unknown")

    def test_kqueue_error_explicit_even_when_identity_absent(self):
        self.record()
        self.native.poll_error = observer.NativeError("queue failed")
        self.native.processes.clear()
        report = self.finish()
        self.assertFalse(report["ok"])
        self.assertIn("kqueue polling failed", " ".join(report["gaps"]))
        self.assertNotEqual(report["helpers"][0]["exit_evidence"], "unknown")

    def test_exit_arriving_before_deadline(self):
        self.record()
        with patch.object(
            observer.time, "sleep", side_effect=lambda _: self.native.events.add(41001)
        ):
            report = self.subject.finish(1)
        self.assertTrue(report["ok"])

    def test_no_records_and_unobserved_roles_explicit(self):
        self.subject.observe("ordinary host output\nnot a CEF marker")
        report = self.finish()
        self.assertFalse(report["ok"])
        self.assertIn("no helper", " ".join(report["gaps"]))
        self.assertTrue(all(r["gaps"] for r in report["roles"].values()))

    def test_launch_start_count_mismatches(self):
        for launches in (0, 2):
            with self.subTest(launches=launches):
                self.setUp_subject()
                self.subject.observe("CEF_HELPER_LAUNCH\n" * launches + start())
                self.subject.observe(ready())
                self.native.events.add(41001)
                report = self.finish()
                self.assertFalse(report["ok"])
                self.assertIn("launch count", " ".join(report["gaps"]))

    def test_duplicates_start_and_ready_not_new_identity_or_queries(self):
        self.record()
        self.subject.observe(start() + "\n" + ready())
        self.native.events.add(41001)
        report = self.finish()
        self.assertFalse(report["ok"])
        self.assertEqual(report["distinct_starts"], 1)
        self.assertEqual(self.native.registered, [41001])
        self.assertEqual(len(self.native.query_calls), 3)
        self.assertIn("duplicate START", " ".join(report["helpers"][0]["gaps"]))
        self.assertIn("duplicate READY", " ".join(report["helpers"][0]["gaps"]))

    def test_ready_before_start_stays_failure(self):
        self.subject.observe(ready())
        self.record()
        self.native.events.add(41001)
        self.assertFalse(self.finish()["ok"])

    def test_malformed_records_and_token_boundaries(self):
        for record in (
            start(pid=0),
            start(pid=-1),
            start(pid="01"),
            start(pid=2**31),
            start(ppid=0),
            start(role="browser"),
            start(subtype="bad/token"),
            start(subtype="x" * 129),
            start(sandbox=""),
            start(seatbelt=2),
            start() + " extra",
            "CEF_HELPER_START pid=41001",
            ready(0),
            ready() + " extra",
            "CEF_HELPER_LAUNCH extra",
            "CEF_HELPER_UNKNOWN",
        ):
            with self.subTest(record=record):
                self.setUp_subject()
                self.subject.observe(record)
                report = self.finish()
                self.assertFalse(report["ok"])
                self.assertEqual(self.native.registered, [])
        self.setUp_subject()
        self.record(subtype="A_.-09" + "x" * 122, sandbox="none", seatbelt=0)
        self.native.events.add(41001)
        self.assertTrue(self.finish()["ok"])

    def test_crlf_batches_and_ignored_unrelated_output(self):
        self.subject.observe(
            "normal stderr\r\nCEF_HELPER_LAUNCH\r\n"
            + start()
            + "\r\n"
            + ready()
            + "\r\n"
        )
        self.native.events.add(41001)
        self.assertTrue(self.finish()["ok"])

    def test_bounded_records_errors_and_oversized_lines(self):
        for i in range(observer.MAX_HELPERS + 20):
            pid = 41001 + i
            self.native.add(pid)
            self.record(pid)
            self.subject.observe("CEF_HELPER_" + "x" * 2000)
        self.native.events = set(self.native.processes)
        report = self.finish()
        self.assertFalse(report["ok"])
        self.assertEqual(report["distinct_starts"], 64)
        self.assertEqual(len(self.native.registered), 64)
        self.assertTrue(all(n <= 65 for n in report["record_counts"].values()))
        self.assertLessEqual(len(report["gaps"]), 64)
        self.assertTrue(any("limit exceeded" in gap for gap in report["gaps"]))
        self.assertTrue(any("line limit" in gap for gap in report["gaps"]))

    def test_native_unavailable_is_reported_not_silently_skipped(self):
        with patch.object(
            observer, "NativeAPI", side_effect=observer.NativeError("API unavailable")
        ):
            subject = observer.HelperObserver(ROOT, APP, CANARY, ALLOWED)
        subject.observe("CEF_HELPER_LAUNCH\n" + start() + "\n" + ready())
        report = subject.finish(0)
        self.assertFalse(report["ok"])
        self.assertIn("API unavailable", " ".join(report["gaps"]))

    def test_canary_open_failure_blocks_policy_evidence(self):
        with patch.object(
            observer, "_open_canary", side_effect=PermissionError("unreadable")
        ):
            subject = observer.HelperObserver(ROOT, APP, CANARY, ALLOWED)
        report = subject.finish(0)
        self.assertFalse(report["ok"])
        self.assertFalse(report["controls"]["canary_open"])
        self.assertIn("unreadable", " ".join(report["gaps"]))

    def test_invalid_inputs_and_missing_framework_control(self):
        for args in (
            (0, APP, CANARY, ALLOWED),
            (True, APP, CANARY, ALLOWED),
            (ROOT, Path("relative.app"), CANARY, ALLOWED),
            (ROOT, Path("/x/../unique.app"), CANARY, ALLOWED),
            (ROOT, Path("/not-an-app"), CANARY, ALLOWED),
            (ROOT, APP, CANARY, Path("/outside/file")),
            (ROOT, APP, ALLOWED, ALLOWED),
        ):
            with self.subTest(args=args):
                report = observer.HelperObserver(*args).finish(0)
                json.dumps(report, allow_nan=False)
                self.assertFalse(report["ok"])
        with patch.object(Path, "is_file", return_value=False):
            subject = observer.HelperObserver(ROOT, APP, CANARY, ALLOWED)
        self.assertFalse(subject.finish(0)["controls"]["allowed_bundle_file"])

    def test_invalid_deadlines_are_bounded_failures(self):
        for seconds in (-1, float("nan"), float("inf"), 10**1000, "3", True):
            with self.subTest(seconds=seconds):
                self.setUp_subject()
                self.record()
                report = self.subject.finish(seconds)
                json.dumps(report, allow_nan=False)
                self.assertFalse(report["ok"])
                self.assertIn("deadline", " ".join(report["gaps"]))

    def test_finish_idempotent_observe_after_finish_rejected(self):
        report = self.finish()
        self.assertIs(self.subject.finish(), report)
        with self.assertRaises(RuntimeError):
            self.subject.observe(start())


class NativeBindingTests(unittest.TestCase):
    def setUp(self):
        self.proc = SimpleNamespace(proc_pidinfo=Mock(), proc_pidpath=Mock())
        self.check = Mock(return_value=1)
        self.system = SimpleNamespace(sandbox_check=self.check)
        self.queue = Mock()
        self.queue.control.return_value = []
        for patcher in (
            patch.object(observer.sys, "platform", "darwin"),
            patch.object(observer.ctypes, "CDLL", side_effect=[self.proc, self.system]),
            patch.object(
                observer.ctypes.c_int, "in_dll", return_value=ctypes.c_int(0x40000000)
            ),
            patch.object(
                observer.select, "kqueue", return_value=self.queue, create=True
            ),
        ):
            patcher.start()
            self.addCleanup(patcher.stop)
        self.api = observer.NativeAPI()
        self.addCleanup(self.api.close)

    def test_sdk_structure_layout(self):
        self.assertEqual(ctypes.sizeof(observer.ProcBsdInfo), 136)
        self.assertEqual(observer.ProcBsdInfo.pbi_comm.offset, 48)
        self.assertEqual(observer.ProcBsdInfo.pbi_nice.offset, 116)
        self.assertEqual(observer.ProcBsdInfo.pbi_start_tvsec.offset, 120)
        self.assertEqual(observer.ProcBsdInfo.pbi_start_tvusec.offset, 128)

    def test_targeted_info_exact_size_identity_and_missing_errors(self):
        def fill(pid, flavor, arg, buffer, size):
            self.assertEqual((pid, flavor, arg, size), (41001, 3, 0, 136))
            info = buffer._obj
            info.pbi_pid, info.pbi_ppid = pid, ROOT
            info.pbi_uid = info.pbi_ruid = info.pbi_svuid = os.getuid()
            info.pbi_start_tvsec, info.pbi_start_tvusec = 100, 99
            info.pbi_status = 2
            return size

        self.proc.proc_pidinfo.side_effect = fill
        self.assertEqual(self.api.info(41001).start, (100, 99))
        self.proc.proc_pidinfo.side_effect = lambda *args: (
            ctypes.set_errno(errno.ESRCH),
            0,
        )[1]
        self.assertIsNone(self.api.info(41001))
        for code, count in ((errno.EPERM, 0), (0, 0), (0, 135)):
            with self.subTest(code=code, count=count):
                self.proc.proc_pidinfo.side_effect = lambda *args: (
                    ctypes.set_errno(code),
                    count,
                )[1]
                with self.assertRaises(observer.NativeError):
                    self.api.info(41001)

    def test_pidpath_errors_not_absence(self):
        def fill(pid, buffer, size):
            self.assertEqual((pid, size), (41001, 4096))
            buffer.value = os.fsencode(helper_path())
            return len(buffer.value)

        self.proc.proc_pidpath.side_effect = fill
        self.assertEqual(self.api.path(41001), helper_path())
        self.proc.proc_pidpath.side_effect = None
        for count in (0, -1, 4096):
            self.proc.proc_pidpath.return_value = count
            with self.assertRaises(observer.NativeError):
                self.api.path(41001)

    def test_variadic_fixed_three_args_and_runtime_no_report_data(self):
        self.assertEqual(
            self.check.argtypes, [ctypes.c_int, ctypes.c_char_p, ctypes.c_int]
        )
        self.api.query(41001)
        self.check.assert_called_once_with(41001, None, 0)
        self.check.reset_mock()
        self.api.query(41001, CANARY)
        args = self.check.call_args.args
        self.assertEqual(args[:3], (41001, b"file-read-data", 0x40000001))
        self.assertIsInstance(args[3], ctypes.c_char_p)
        self.assertEqual(args[3].value, os.fsencode(CANARY))

    def test_negative_unexpected_and_errno_results_are_not_denial(self):
        for result, code in ((-1, 0), (2, 0), (1, errno.EPERM), (0, errno.ESRCH)):
            with self.subTest(result=result, code=code):
                self.check.side_effect = lambda *args: (ctypes.set_errno(code), result)[
                    1
                ]
                with self.assertRaises(observer.NativeError):
                    self.api.query(41001, CANARY)

    def test_unavailable_policy_symbol_explicit(self):
        for missing in ("function", "data"):
            with self.subTest(missing=missing):
                system = SimpleNamespace() if missing == "function" else self.system
                with (
                    patch.object(
                        observer.ctypes, "CDLL", side_effect=[self.proc, system]
                    ),
                    patch.object(
                        observer.ctypes.c_int,
                        "in_dll",
                        side_effect=ValueError("missing data symbol"),
                    ),
                ):
                    api = observer.NativeAPI()
                self.addCleanup(api.close)
                with self.assertRaisesRegex(observer.NativeError, "unavailable"):
                    api.query(41001, CANARY)
                self.check.assert_not_called()

    def test_invalid_no_report_data_is_explicit_failure(self):
        for value in (0, 1, 3):
            with self.subTest(value=value):
                with (
                    patch.object(
                        observer.ctypes, "CDLL", side_effect=[self.proc, self.system]
                    ),
                    patch.object(
                        observer.ctypes.c_int,
                        "in_dll",
                        return_value=ctypes.c_int(value),
                    ),
                ):
                    api = observer.NativeAPI()
                self.addCleanup(api.close)
                with self.assertRaisesRegex(observer.NativeError, "NO_REPORT"):
                    api.query(41001, CANARY)

    @unittest.skipUnless(
        hasattr(observer.select, "kevent"), "kqueue constants require macOS/BSD"
    )
    def test_kqueue_registration_preserves_other_helpers_exit_events(self):
        event = SimpleNamespace(
            flags=0,
            filter=observer.select.KQ_FILTER_PROC,
            fflags=observer.select.KQ_NOTE_EXIT,
            ident=41001,
        )
        self.queue.control.side_effect = [[event], []]
        self.api.register(41002)
        change = self.queue.control.call_args.args[0][0]
        self.assertEqual(change.ident, 41002)
        self.assertEqual(change.filter, observer.select.KQ_FILTER_PROC)
        self.assertEqual(change.fflags, observer.select.KQ_NOTE_EXIT)
        self.assertTrue(change.flags & observer.select.KQ_EV_ADD)
        self.assertEqual(self.api.poll(0), {41001})

    @unittest.skipUnless(
        hasattr(observer.select, "kevent"), "kqueue constants require macOS/BSD"
    )
    def test_kqueue_error_never_counted_as_exit(self):
        event = SimpleNamespace(flags=observer.select.KQ_EV_ERROR, data=errno.ESRCH)
        self.queue.control.return_value = [event]
        with self.assertRaisesRegex(observer.NativeError, "EV_ERROR"):
            self.api.register(41001)

    def test_platform_or_library_unavailable_explicit(self):
        with patch.object(observer.sys, "platform", "linux"):
            with self.assertRaisesRegex(observer.NativeError, "unavailable"):
                observer.NativeAPI()
        with patch.object(observer.ctypes, "CDLL", side_effect=OSError("not found")):
            with self.assertRaisesRegex(observer.NativeError, "unavailable"):
                observer.NativeAPI()

    def test_open_canary_exact_read_only_regular_file_and_close(self):
        with (
            patch.object(observer.os, "open", return_value=77) as opening,
            patch.object(
                observer.os, "fstat", return_value=SimpleNamespace(st_mode=0o100600)
            ),
            patch.object(observer.os, "close") as closing,
        ):
            observer._open_canary(CANARY)
            opening.assert_called_once_with(
                CANARY, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
            )
            closing.assert_called_once_with(77)
        with (
            patch.object(observer.os, "open", return_value=78),
            patch.object(
                observer.os, "fstat", return_value=SimpleNamespace(st_mode=0o010600)
            ),
            patch.object(observer.os, "close") as closing,
        ):
            with self.assertRaises(observer.NativeError):
                observer._open_canary(CANARY)
            closing.assert_called_once_with(78)


if __name__ == "__main__":
    unittest.main()
