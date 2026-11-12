#!/usr/bin/env python3
"""Explicit, stdlib-only preparation of the pinned macOS arm64 CEF SDK.

No engine launch, signing, quarantine changes, or sandbox configuration. SHA-1
checks the publisher's index, not publisher authenticity; SHA-256 is recorded
locally. The private manifest detects subsequent changes, not a hostile owner.
"""

import argparse
import bz2
import hashlib
import json
import os
import re
import shutil
import signal
import stat
import struct
import tarfile
import tempfile
import time
import urllib.request
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

VERSION = "156.0.26+g8ea0c73+chromium-156.0.8078.88"
NAME = f"cef_binary_{VERSION}_macosarm64_minimal.tar.bz2"
URL = "https://cef-builds.spotifycdn.com/" + NAME
SHA1 = "ca4fafe7a9c52d9f28200bb8713971a9662b891c"
COMPRESSED_BYTES = 134979837
DEFAULT_ROOT = Path(__file__).resolve().parents[2] / "target/cef-runtime"
MARKER = ".felinaut-cef-prepared.json"
FRAMEWORK = "Release/Chromium Embedded Framework.framework"
BINARY = FRAMEWORK + "/Chromium Embedded Framework"
SANDBOX = FRAMEWORK + "/Libraries/libcef_sandbox.dylib"
CHUNK = 1024 * 1024


class PreparationError(ValueError):
    pass


@dataclass(frozen=True)
class Pin:
    version: str = VERSION
    name: str = NAME
    sha1: str = SHA1
    size: int = COMPRESSED_BYTES
    url: str = URL

    @property
    def top(self):
        return self.name.removesuffix(".tar.bz2")

    @property
    def archive_json(self):
        return {"type": "minimal", "name": self.name, "sha1": self.sha1}


@dataclass(frozen=True)
class Limits:
    members: int = 50000
    file_bytes: int = 2 * 1024**3
    extracted_bytes: int = 8 * 1024**3
    tar_bytes: int = 9 * 1024**3
    reserve_bytes: int = 1024**3


class Deadline:
    def __init__(self, seconds=180):
        self.end = time.monotonic() + seconds

    def check(self):
        remaining = self.end - time.monotonic()
        if remaining <= 0:
            raise PreparationError("Preparation time budget exhausted")
        return remaining


def private_directory(path):
    """Never follow symlinks in the output ancestry or modify existing modes."""
    path = Path(os.path.abspath(path))
    missing = []
    ancestor = path
    while not ancestor.exists() and not ancestor.is_symlink():
        missing.append(ancestor)
        ancestor = ancestor.parent
    for component in (ancestor, *ancestor.parents):
        if component.is_symlink() or not component.is_dir():
            raise PreparationError(f"Unsafe output ancestor: {component}")
    for directory in reversed(missing):
        directory.mkdir(mode=0o700)
    info = path.lstat()
    if (
        not stat.S_ISDIR(info.st_mode)
        or info.st_uid != os.getuid()
        or stat.S_IMODE(info.st_mode) & 0o077
    ):
        raise PreparationError(
            f"Output directory must be owned and private (0700): {path}"
        )
    return path


def regular_file(path):
    if not stat.S_ISREG(path.lstat().st_mode):
        raise PreparationError(f"Not a regular file: {path}")


def hashes(path, deadline, expected_size=None):
    regular_file(path)
    if expected_size is not None and path.stat().st_size != expected_size:
        raise PreparationError("Compressed archive size mismatch")
    sha1, sha256 = hashlib.sha1(), hashlib.sha256()
    with path.open("rb") as source:
        while data := source.read(CHUNK):
            deadline.check()
            sha1.update(data)
            sha256.update(data)
    return sha1.hexdigest(), sha256.hexdigest()


def verify_archive(path, pin, deadline):
    sha1, sha256 = hashes(path, deadline, pin.size)
    if sha1 != pin.sha1:
        raise PreparationError("Publisher SHA-1 checksum mismatch")
    return sha256


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise PreparationError("Archive redirects are not permitted")


def download(path, pin, deadline, opener=None):
    """One HTTPS request, no retries, exact length and a cumulative deadline."""
    if not pin.url.startswith("https://cef-builds.spotifycdn.com/"):
        raise PreparationError("Only the pinned official HTTPS origin is allowed")
    if path.exists() or path.is_symlink():
        return verify_archive(path, pin, deadline)
    opener = opener or urllib.request.build_opener(NoRedirect())
    request = urllib.request.Request(pin.url, headers={"Accept-Encoding": "identity"})
    part = None
    try:
        with opener.open(request, timeout=min(15, deadline.check())) as response:
            if response.status != 200 or response.geturl() != pin.url:
                raise PreparationError("Unexpected archive HTTP response")
            if response.headers.get("Content-Encoding", "identity") != "identity":
                raise PreparationError("Unexpected HTTP content encoding")
            if response.headers.get("Content-Length") != str(pin.size):
                raise PreparationError("HTTP Content-Length does not match exact pin")
            fd, temporary = tempfile.mkstemp(prefix=".download-", dir=path.parent)
            part = Path(temporary)
            count = 0
            with os.fdopen(fd, "wb") as target:
                while True:
                    deadline.check()
                    data = response.read(min(CHUNK, pin.size - count + 1))
                    if not data:
                        break
                    count += len(data)
                    if count > pin.size:
                        raise PreparationError("Download exceeds compressed byte cap")
                    target.write(data)
                target.flush()
                os.fsync(target.fileno())
            digest = verify_archive(part, pin, deadline)
            # A hard link publishes without ever overwriting an existing path.
            os.link(part, path)
            return digest
    finally:
        if part is not None:
            part.unlink(missing_ok=True)


class BoundedTarReader:
    def __init__(self, source, limits, deadline):
        self.source, self.limits, self.deadline = source, limits, deadline
        self.count = 0

    def read(self, size):
        self.deadline.check()
        # In particular, do not let a forged PAX header request a huge allocation.
        if size < 0 or size > 16 * CHUNK:
            raise PreparationError("Oversized tar metadata read")
        data = self.source.read(size)
        self.count += len(data)
        if self.count > self.limits.tar_bytes:
            raise PreparationError("Uncompressed tar stream exceeds byte cap")
        return data


def members(path, limits, deadline):
    class BoundedTarInfo(tarfile.TarInfo):
        def _proc_member(self, archive):
            # Guard before tarfile parses/allocates extended-header payloads.
            if self.size < 0 or self.size > limits.file_bytes:
                raise PreparationError("Tar member size exceeds cap")
            if self.type in (
                tarfile.XHDTYPE,
                tarfile.XGLTYPE,
                tarfile.GNUTYPE_LONGNAME,
                tarfile.GNUTYPE_LONGLINK,
            ):
                if self.size > CHUNK:
                    raise PreparationError("Oversized extended tar metadata")
            if self.type == tarfile.GNUTYPE_SPARSE:
                raise PreparationError("Sparse files are forbidden")
            return super()._proc_member(archive)

    with bz2.BZ2File(path, "rb") as source:
        bounded = BoundedTarReader(source, limits, deadline)
        with tarfile.open(
            fileobj=bounded, mode="r|", tarinfo=BoundedTarInfo
        ) as archive:
            for index, member in enumerate(archive):
                if index >= limits.members:
                    raise PreparationError("Tar member count exceeds cap")
                yield archive, member


def relative_name(member, pin):
    name = member.name.rstrip("/") if member.isdir() else member.name
    parts = name.split("/")
    if (
        not name
        or len(name) > 2048
        or "\\" in name
        or "\x00" in name
        or any(p in ("", ".", "..") for p in parts)
        or parts[0] != pin.top
    ):
        raise PreparationError(f"Unsafe archive path: {name!r}")
    relative = "/".join(parts[1:])
    if not relative and not member.isdir():
        raise PreparationError("Archive root is not a directory")
    if relative in (MARKER, "archive.json"):
        raise PreparationError("Archive collides with preparation metadata")
    return relative


def contained_target(name, target):
    if not target or target.startswith("/") or "\\" in target or "\x00" in target:
        raise PreparationError(f"Unsafe symlink target: {target!r}")
    parts = list(PurePosixPath(name).parent.parts)
    for part in target.split("/"):
        if part == "..":
            if not parts:
                raise PreparationError("Symlink escapes SDK root")
            parts.pop()
        elif part not in ("", "."):
            parts.append(part)
    return "/".join(parts)


def preflight(path, pin, limits, deadline):
    plan = {}
    total = 0
    for _, member in members(path, limits, deadline):
        name = relative_name(member, pin)
        if name in plan:
            raise PreparationError(f"Duplicate tar path: {name}")
        if member.islnk() or not (member.isdir() or member.isfile() or member.issym()):
            raise PreparationError(
                "Hardlinks, devices, FIFOs and special entries are forbidden"
            )
        if member.mode & ~0o777:
            raise PreparationError("Special file modes are forbidden")
        if member.size < 0 or member.size > limits.file_bytes:
            raise PreparationError("Tar member size exceeds cap")
        if not member.isfile() and member.size:
            raise PreparationError("Non-file member contains data")
        if member.sparse is not None:
            raise PreparationError("Sparse files are forbidden")
        total += member.size
        if total > limits.extracted_bytes:
            raise PreparationError("Extracted size exceeds cap")
        kind = "dir" if member.isdir() else "file" if member.isfile() else "symlink"
        entry = {"kind": kind, "mode": member.mode & 0o777, "size": member.size}
        if member.issym():
            contained_target(name, member.linkname)
            entry["target"] = member.linkname
        plan[name] = entry
    if not plan:
        raise PreparationError("Empty archive")
    for name, entry in list(plan.items()):
        for parent in PurePosixPath(name).parents:
            key = str(parent)
            if key == ".":
                continue
            if key in plan and plan[key]["kind"] != "dir":
                raise PreparationError(
                    "Tar member traverses a link or non-directory parent"
                )
            plan.setdefault(key, {"kind": "dir", "mode": 0o755, "size": 0})
    for name, entry in plan.items():
        if entry["kind"] == "symlink":
            # Resolve all target components, including intermediate symlinks.
            todo = list(PurePosixPath(name).parent.parts) + entry["target"].split("/")
            resolved = []
            hops = 0
            while todo:
                component = todo.pop(0)
                if component in ("", "."):
                    continue
                if component == "..":
                    if not resolved:
                        raise PreparationError("Resolved symlink escapes SDK root")
                    resolved.pop()
                    continue
                resolved.append(component)
                key = "/".join(resolved)
                linked = plan.get(key)
                if linked and linked["kind"] == "symlink":
                    hops += 1
                    if hops > 40:
                        raise PreparationError("Cyclic or excessive symlink chain")
                    todo = linked["target"].split("/") + todo
                    resolved.pop()
            if "/".join(resolved) not in plan:
                raise PreparationError("Dangling symlink target")
    return plan, total


def write_json(path, value):
    with path.open("x", encoding="utf-8") as target:
        json.dump(value, target, sort_keys=True, indent=2)
        target.write("\n")


def validate_sdk(root, pin):
    header = root / "include/cef_version.h"
    regular_file(header)
    if header.stat().st_size > CHUNK:
        raise PreparationError("Oversized version header")
    versions = re.findall(
        r'^\s*#define\s+CEF_VERSION\s+"([^"\n]+)"\s*$', header.read_text(), re.MULTILINE
    )
    if versions != [pin.version]:
        raise PreparationError("Full CEF_VERSION does not match exact pin")
    for name in (BINARY, SANDBOX):
        path = root / name
        resolved = path.resolve(strict=True)
        if not resolved.is_relative_to(root.resolve()):
            raise PreparationError("Framework binary escapes SDK")
        regular_file(resolved)
        with resolved.open("rb") as source:
            header = source.read(32)
        # Thin little-endian MH_MAGIC_64, CPU_TYPE_ARM64, MH_DYLIB.
        if len(header) != 32 or struct.unpack("<IIII", header[:16]) != (
            0xFEEDFACF,
            0x0100000C,
            0,
            6,
        ):
            raise PreparationError(f"Not the expected arm64 Mach-O dylib: {name}")
        if not resolved.stat().st_size > 32:
            raise PreparationError("Truncated Mach-O dylib")


def extract(path, stage, pin, plan, limits, deadline):
    # Create ordinary directories first and links last: no extraction through links.
    for name, entry in sorted(plan.items(), key=lambda item: item[0].count("/")):
        if name and entry["kind"] == "dir":
            (stage / name).mkdir(mode=0o700, exist_ok=True)
    for archive, member in members(path, limits, deadline):
        name = relative_name(member, pin)
        if not member.isfile():
            continue
        entry = plan[name]
        digest, count = hashlib.sha256(), 0
        with archive.extractfile(member) as source, (stage / name).open("xb") as target:
            while data := source.read(CHUNK):
                deadline.check()
                count += len(data)
                if count > entry["size"]:
                    raise PreparationError("Extracted file exceeds preflight size")
                digest.update(data)
                target.write(data)
        if count != entry["size"]:
            raise PreparationError("Truncated extracted file")
        entry["sha256"] = digest.hexdigest()
        (stage / name).chmod(entry["mode"])
    for name, entry in plan.items():
        if entry["kind"] == "symlink":
            (stage / name).symlink_to(entry["target"])
    for name, entry in plan.items():
        if name and entry["kind"] == "dir":
            (stage / name).chmod(entry["mode"])


def load_json(path):
    regular_file(path)
    if path.stat().st_size > 16 * CHUNK:
        raise PreparationError("Oversized preparation metadata")
    return json.loads(path.read_text())


def verify_tree(destination, pin, digest, deadline):
    private_directory(destination)
    marker = load_json(destination / MARKER)
    if (
        marker.get("format") != "felinaut-cef-preparation-v1"
        or marker.get("pin") != pin.archive_json
        or marker.get("version") != pin.version
        or marker.get("compressed_bytes") != pin.size
        or marker.get("sha256") != digest
    ):
        raise PreparationError(
            "Destination has no matching verified preparation marker"
        )
    if load_json(destination / "archive.json") != pin.archive_json:
        raise PreparationError("archive.json does not match exact pin")
    manifest = marker["manifest"]
    actual = set()
    for parent, directories, files in os.walk(destination, followlinks=False):
        deadline.check()
        for leaf in directories + files:
            path = Path(parent) / leaf
            name = path.relative_to(destination).as_posix()
            if name in (MARKER, "archive.json"):
                continue
            actual.add(name)
            entry = manifest.get(name)
            if entry is None:
                raise PreparationError(f"Unrecorded SDK path: {name}")
            info = path.lstat()
            kind = (
                "symlink"
                if stat.S_ISLNK(info.st_mode)
                else "dir"
                if stat.S_ISDIR(info.st_mode)
                else "file"
                if stat.S_ISREG(info.st_mode)
                else "special"
            )
            if kind != entry["kind"]:
                raise PreparationError(f"SDK entry type changed: {name}")
            if kind == "symlink":
                if os.readlink(path) != entry["target"]:
                    raise PreparationError(f"SDK symlink changed: {name}")
            elif stat.S_IMODE(info.st_mode) != entry["mode"]:
                raise PreparationError(f"SDK mode changed: {name}")
            elif kind == "file":
                if (
                    info.st_size != entry["size"]
                    or hashes(path, deadline)[1] != entry["sha256"]
                ):
                    raise PreparationError(f"SDK file changed: {name}")
    if actual != set(manifest) - {""}:
        raise PreparationError("SDK manifest entries missing")
    validate_sdk(destination, pin)
    return marker


def prepare(root, acquire=False, pin=Pin(), limits=Limits(), deadline=None):
    deadline = deadline or Deadline()
    root = private_directory(root)
    version_root = private_directory(root / pin.version.split("+")[0])
    destination = version_root / "macosarm64"
    archive = root / pin.name
    if destination.exists() or destination.is_symlink():
        # Refuse unknown trees before even considering a network request.
        private_directory(destination)
        load_json(destination / MARKER)
        digest = verify_archive(archive, pin, deadline)
        return destination, verify_tree(destination, pin, digest, deadline)
    if not acquire:
        raise PreparationError("No prepared SDK; run explicitly with --download")
    needed = limits.extracted_bytes + limits.reserve_bytes + 2 * pin.size
    if shutil.disk_usage(root).free < needed:
        raise PreparationError(f"Insufficient disk budget; require {needed} free bytes")
    digest = download(archive, pin, deadline)
    plan, total = preflight(archive, pin, limits, deadline)
    stage = Path(tempfile.mkdtemp(prefix=".prepare-", dir=version_root))
    try:
        extract(archive, stage, pin, plan, limits, deadline)
        validate_sdk(stage, pin)
        write_json(stage / "archive.json", pin.archive_json)
        marker = {
            "format": "felinaut-cef-preparation-v1",
            "pin": pin.archive_json,
            "version": pin.version,
            "sha256": digest,
            "compressed_bytes": pin.size,
            "extracted_bytes": total,
            "manifest": plan,
        }
        write_json(stage / MARKER, marker)
        verify_tree(stage, pin, digest, deadline)
        if destination.exists() or destination.is_symlink():
            raise PreparationError(
                "Destination appeared during preparation; refusing overwrite"
            )
        # A private, exclusive lock held by the CLI serializes cooperating invocations.
        stage.rename(destination)
        return destination, marker
    finally:
        if stage.exists():
            shutil.rmtree(stage)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument(
        "--download",
        action="store_true",
        help="explicitly acquire the exact pinned archive",
    )
    mode.add_argument(
        "--verify",
        action="store_true",
        help="offline full verification; never downloads",
    )
    parser.add_argument(
        "--root", type=Path, default=DEFAULT_ROOT, help="private artifact root"
    )
    args = parser.parse_args(argv)
    if not (args.download or args.verify):
        parser.print_help()
        return 0
    lock = None
    try:
        root = private_directory(args.root)
        lock = root / ".prepare.lock"
        fd = os.open(lock, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.close(fd)
    except (OSError, PreparationError) as error:
        parser.exit(1, f"Preparation refused: {error}\n")

    def expired(signum, frame):
        raise PreparationError("Preparation time budget exhausted (180 seconds)")

    previous_handler = signal.signal(signal.SIGALRM, expired)
    signal.alarm(180)
    try:
        destination, record = prepare(root, acquire=args.download)
        print(f"CEF_PATH={destination}")
        print(f"CEF_VERSION={record['version']}")
        print(f"compressed_bytes={record['compressed_bytes']}")
        print(f"extracted_bytes={record['extracted_bytes']}")
        print(f"SHA1={record['pin']['sha1']} (publisher checksum, not authentication)")
        print(f"SHA256={record['sha256']}")
        print(
            "Exact pin, tree hashes and arm64 dylib headers verified; not full security validation."
        )
        return 0
    except (
        OSError,
        ValueError,
        KeyError,
        TypeError,
        tarfile.TarError,
        EOFError,
    ) as error:
        parser.exit(1, f"Preparation failed: {error}\n")
    finally:
        signal.alarm(0)
        signal.signal(signal.SIGALRM, previous_handler)
        lock.unlink()


if __name__ == "__main__":
    raise SystemExit(main())
