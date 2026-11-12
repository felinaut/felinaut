"""Behavioral offline tests; synthetic SDKs are never loaded or executed."""

import bz2
import hashlib
import io
import json
import os
import struct
import tarfile
import tempfile
import unittest
from contextlib import redirect_stdout
from dataclasses import replace
from pathlib import Path
from unittest.mock import patch

import prepare_runtime as runtime


class Response(io.BytesIO):
    def __init__(self, data, pin):
        super().__init__(data)
        self.status = 200
        self.headers = {"Content-Length": str(pin.size)}
        self.url = pin.url

    def geturl(self):
        return self.url


class Opener:
    def __init__(self, response):
        self.response = response
        self.calls = 0

    def open(self, request, timeout):
        self.calls += 1
        if not 0 < timeout <= 15:
            raise AssertionError("Missing bounded timeout")
        return self.response


class PreparationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)
        self.root = self.base / "runtime"
        self.root.mkdir(mode=0o700)
        self.pin = runtime.Pin()
        self.limits = runtime.Limits(extracted_bytes=1024 * 1024, reserve_bytes=0)
        self.macho = (
            struct.pack("<IIIIIIII", 0xFEEDFACF, 0x0100000C, 0, 6, 0, 0, 0, 0)
            + b"fixture"
        )

    def fixture(self):
        framework = runtime.FRAMEWORK
        return [
            (
                "include/cef_version.h",
                f'#define CEF_VERSION "{self.pin.version}"\n'.encode(),
            ),
            (
                framework + "/Versions/A/Chromium Embedded Framework",
                self.macho,
                "file",
                0o755,
            ),
            (
                framework + "/Versions/A/Libraries/libcef_sandbox.dylib",
                self.macho,
                "file",
                0o755,
            ),
            (framework + "/Versions/Current", "A", "symlink"),
            (
                framework + "/Chromium Embedded Framework",
                "Versions/Current/Chromium Embedded Framework",
                "symlink",
            ),
            (framework + "/Libraries", "Versions/Current/Libraries", "symlink"),
        ]

    def archive(self, entries=None):
        entries = self.fixture() if entries is None else entries
        target = self.root / self.pin.name
        with tarfile.open(target, "w:bz2", format=tarfile.PAX_FORMAT) as archive:
            for item in entries:
                name, value, *extra = item
                kind = extra[0] if extra else "file"
                info = tarfile.TarInfo(self.pin.top + "/" + name)
                info.mode = (
                    extra[1] if len(extra) > 1 else 0o755 if kind == "dir" else 0o644
                )
                if kind == "file":
                    info.size = len(value)
                    archive.addfile(info, io.BytesIO(value))
                else:
                    info.type = {
                        "symlink": tarfile.SYMTYPE,
                        "hardlink": tarfile.LNKTYPE,
                        "dir": tarfile.DIRTYPE,
                        "fifo": tarfile.FIFOTYPE,
                        "device": tarfile.CHRTYPE,
                    }[kind]
                    info.linkname = value
                    archive.addfile(info)
        data = target.read_bytes()
        self.pin = replace(
            self.pin, size=len(data), sha1=hashlib.sha1(data).hexdigest()
        )
        return target

    def preflight(self):
        return runtime.preflight(
            self.root / self.pin.name, self.pin, self.limits, runtime.Deadline()
        )

    def prepare(self):
        return runtime.prepare(self.root, True, self.pin, self.limits)

    def rejected(self, entries, pattern=None):
        self.archive(entries)
        with self.assertRaisesRegex(runtime.PreparationError, pattern or "."):
            self.preflight()

    def test_success_framework_links_modes_metadata_and_offline_reuse(self):
        archive = self.archive()
        destination, record = self.prepare()
        self.assertEqual(destination, self.root / "156.0.26/macosarm64")
        self.assertEqual(
            record["sha256"], hashlib.sha256(archive.read_bytes()).hexdigest()
        )
        self.assertEqual(
            record["extracted_bytes"],
            sum(len(x[1]) for x in self.fixture() if isinstance(x[1], bytes)),
        )
        self.assertEqual(
            json.loads((destination / "archive.json").read_text()),
            self.pin.archive_json,
        )
        self.assertEqual((destination / runtime.BINARY).read_bytes(), self.macho)
        self.assertEqual((destination / runtime.BINARY).stat().st_mode & 0o777, 0o755)
        self.assertEqual(destination.stat().st_mode & 0o777, 0o700)
        with patch(
            "urllib.request.build_opener",
            side_effect=AssertionError("network forbidden"),
        ):
            reused, second = runtime.prepare(self.root, False, self.pin, self.limits)
        self.assertEqual((destination, record), (reused, second))
        self.assertFalse(list(destination.parent.glob(".prepare-*")))

    def test_default_cli_no_network_or_disk(self):
        with (
            patch("urllib.request.build_opener", side_effect=AssertionError("network")),
            redirect_stdout(io.StringIO()),
        ):
            self.assertEqual(runtime.main([]), 0)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_verify_never_downloads_missing_archive(self):
        with patch(
            "urllib.request.build_opener", side_effect=AssertionError("network")
        ):
            with self.assertRaises(runtime.PreparationError):
                runtime.prepare(self.root, False, self.pin, self.limits)

    def test_arbitrary_destination_preserved(self):
        destination = self.root / "156.0.26/macosarm64"
        destination.parent.mkdir(mode=0o700)
        destination.mkdir(mode=0o700)
        sentinel = destination / "unrelated"
        sentinel.write_text("keep")
        with patch(
            "urllib.request.build_opener", side_effect=AssertionError("network")
        ):
            with self.assertRaises(OSError):
                self.prepare()
        self.assertEqual(sentinel.read_text(), "keep")

    def test_existing_destination_symlink_refused(self):
        (self.root / "156.0.26").mkdir(mode=0o700)
        (self.root / "156.0.26/macosarm64").symlink_to(
            self.base, target_is_directory=True
        )
        with self.assertRaises(runtime.PreparationError):
            self.prepare()

    def test_private_root_required(self):
        self.root.chmod(0o755)
        with self.assertRaises(runtime.PreparationError):
            self.prepare()

    def test_symlink_output_ancestor_refused(self):
        link = self.base / "link"
        link.symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(runtime.PreparationError):
            runtime.private_directory(link / "child")
        self.assertFalse((self.root / "child").exists())

    def test_checksum_mismatch_preserves_archive_no_destination(self):
        archive = self.archive()
        self.pin = replace(self.pin, sha1="0" * 40)
        with self.assertRaisesRegex(runtime.PreparationError, "checksum"):
            self.prepare()
        self.assertTrue(archive.exists())
        self.assertFalse((self.root / "156.0.26/macosarm64").exists())

    def test_wrong_compressed_length(self):
        self.archive()
        self.pin = replace(self.pin, size=self.pin.size + 1)
        with self.assertRaisesRegex(runtime.PreparationError, "size mismatch"):
            self.prepare()

    def test_invalid_bzip_archive(self):
        archive = self.archive()
        archive.write_bytes(b"not a tar")
        with self.assertRaises((OSError, tarfile.TarError)):
            self.preflight()

    def test_empty_archive(self):
        self.rejected([], "Empty")

    def test_traversal_and_invalid_paths(self):
        for name in (
            "../escape",
            "a/../../escape",
            "/absolute",
            "a//b",
            "a/./b",
            "a\\b",
        ):
            with self.subTest(name=name):
                self.rejected([(name, b"bad")], "Unsafe archive path")
        self.assertFalse((self.base / "escape").exists())

    def test_absolute_tar_path_and_wrong_top(self):
        for name in ("/tmp/absolute", "other-root/file"):
            with self.subTest(name=name):
                info = tarfile.TarInfo(name)
                with tarfile.open(self.root / self.pin.name, "w:bz2") as archive:
                    archive.addfile(info)
                with self.assertRaises(runtime.PreparationError):
                    self.preflight()

    def test_duplicate_and_metadata_paths(self):
        for entries in (
            [("x", b"a"), ("x", b"b")],
            [("archive.json", b"x")],
            [(runtime.MARKER, b"x")],
        ):
            with self.subTest(entries=entries):
                self.rejected(entries)

    def test_link_escape_and_absolute_link(self):
        for target in ("../../outside", "/outside", "a\\b"):
            with self.subTest(target=target):
                self.rejected([("dir/link", target, "symlink")])

    def test_link_parent_before_or_after_child(self):
        link = ("linked", "real", "symlink")
        file = ("linked/file", b"bad")
        for entries in (
            [link, file, ("real", "", "dir")],
            [file, link, ("real", "", "dir")],
        ):
            with self.subTest(entries=entries):
                self.rejected(entries, "parent")

    def test_chained_link_dotdot_escape(self):
        self.rejected(
            [
                ("rootlink", ".", "symlink"),
                ("dir/link", "../rootlink/../outside", "symlink"),
            ]
        )

    def test_cyclic_and_dangling_links(self):
        for entries in (
            [("a", "b", "symlink"), ("b", "a", "symlink")],
            [("a", "missing", "symlink")],
        ):
            with self.subTest(entries=entries):
                self.rejected(entries)

    def test_hardlinks_devices_fifos(self):
        for kind in ("hardlink", "device", "fifo"):
            with self.subTest(kind=kind):
                self.rejected([("special", "target", kind)], "forbidden")

    def test_special_modes(self):
        self.rejected([("setuid", b"x", "file", 0o4755)], "modes")

    def test_file_total_and_count_limits(self):
        self.archive([("a", b"1234"), ("b", b"5678")])
        for limits in (
            replace(self.limits, file_bytes=3),
            replace(self.limits, extracted_bytes=7),
            replace(self.limits, members=1),
        ):
            with self.subTest(limits=limits):
                with self.assertRaises(runtime.PreparationError):
                    runtime.preflight(
                        self.root / self.pin.name, self.pin, limits, runtime.Deadline()
                    )
        self.limits = replace(self.limits, file_bytes=4, extracted_bytes=8, members=2)
        self.assertEqual(self.preflight()[1], 8)

    def test_uncompressed_tar_budget(self):
        self.archive([("a", b"x" * 50000)])
        self.limits = replace(self.limits, tar_bytes=1024)
        with self.assertRaisesRegex(runtime.PreparationError, "stream"):
            self.preflight()

    def test_oversized_pax_header_rejected_before_payload(self):
        info = tarfile.TarInfo("pax")
        info.type = tarfile.XHDTYPE
        info.size = 2 * runtime.CHUNK
        (self.root / self.pin.name).write_bytes(bz2.compress(info.tobuf()))
        with self.assertRaisesRegex(runtime.PreparationError, "metadata"):
            self.preflight()

    def test_wrong_version_or_architecture_or_missing_sandbox_cleans_stage(self):
        for change in ("version", "arch", "sandbox"):
            with self.subTest(change=change):
                entries = self.fixture()
                if change == "version":
                    entries[0] = (entries[0][0], b'#define CEF_VERSION "156.0.25"\n')
                elif change == "arch":
                    entries[1] = (entries[1][0], b"not a Mach-O dylib" * 3)
                else:
                    entries = entries[:2]
                self.archive(entries)
                with self.assertRaises((runtime.PreparationError, OSError)):
                    self.prepare()
                self.assertFalse((self.root / "156.0.26/macosarm64").exists())
                self.assertFalse(list((self.root / "156.0.26").glob(".prepare-*")))

    def test_mutated_tree_refused(self):
        self.archive()
        destination, _ = self.prepare()
        binary = destination / runtime.BINARY
        binary.write_bytes(self.macho + b"tampering")
        with self.assertRaisesRegex(runtime.PreparationError, "file changed"):
            self.prepare()
        self.assertEqual(binary.read_bytes(), self.macho + b"tampering")

    def test_extra_missing_and_changed_symlink_refused(self):
        self.archive()
        destination, _ = self.prepare()
        extra = destination / "extra"
        extra.write_text("bad")
        with self.assertRaisesRegex(runtime.PreparationError, "Unrecorded"):
            self.prepare()
        extra.unlink()
        link = destination / runtime.FRAMEWORK / "Versions/Current"
        link.unlink()
        link.symlink_to("B")
        with self.assertRaisesRegex(runtime.PreparationError, "symlink changed"):
            self.prepare()
        link.unlink()
        with self.assertRaisesRegex(runtime.PreparationError, "entries missing"):
            self.prepare()

    def test_older_or_wrong_own_marker_refused(self):
        self.archive()
        destination, _ = self.prepare()
        path = destination / runtime.MARKER
        marker = json.loads(path.read_text())
        marker["version"] = "156.0.25"
        path.write_text(json.dumps(marker))
        with self.assertRaisesRegex(runtime.PreparationError, "marker"):
            self.prepare()

    def test_archive_json_exact_not_older(self):
        self.archive()
        destination, _ = self.prepare()
        (destination / "archive.json").write_text(
            json.dumps(
                {
                    "type": "minimal",
                    "name": "cef_binary_156.0.25",
                    "sha1": self.pin.sha1,
                }
            )
        )
        with self.assertRaisesRegex(runtime.PreparationError, "archive.json"):
            self.prepare()

    def test_disk_and_deadline_budget(self):
        self.archive()
        with patch("shutil.disk_usage", return_value=type("Usage", (), {"free": 0})()):
            with self.assertRaisesRegex(runtime.PreparationError, "disk budget"):
                self.prepare()
        with self.assertRaisesRegex(runtime.PreparationError, "time budget"):
            runtime.prepare(
                self.root, True, self.pin, self.limits, runtime.Deadline(-1)
            )

    def test_download_success_and_cache_reuse(self):
        archive = self.archive()
        data = archive.read_bytes()
        archive.unlink()
        opener = Opener(Response(data, self.pin))
        digest = runtime.download(archive, self.pin, runtime.Deadline(), opener)
        self.assertEqual(digest, hashlib.sha256(data).hexdigest())
        self.assertEqual(archive.stat().st_mode & 0o777, 0o600)
        runtime.download(archive, self.pin, runtime.Deadline(), opener)
        self.assertEqual(opener.calls, 1)
        self.assertFalse(list(self.root.glob(".download-*")))

    def test_download_bad_length_hash_headers_status_url_and_encoding(self):
        archive = self.archive()
        data = archive.read_bytes()
        archive.unlink()
        for change in (
            "short",
            "long",
            "hash",
            "length",
            "status",
            "redirect",
            "encoding",
        ):
            with self.subTest(change=change):
                body = (
                    data[:-1]
                    if change == "short"
                    else data + b"x"
                    if change == "long"
                    else b"x" * len(data)
                    if change == "hash"
                    else data
                )
                response = Response(body, self.pin)
                if change == "length":
                    response.headers["Content-Length"] = "1"
                elif change == "status":
                    response.status = 206
                elif change == "redirect":
                    response.url = "https://other.invalid/archive"
                elif change == "encoding":
                    response.headers["Content-Encoding"] = "gzip"
                with self.assertRaises(runtime.PreparationError):
                    runtime.download(
                        archive, self.pin, runtime.Deadline(), Opener(response)
                    )
                self.assertFalse(archive.exists())
                self.assertFalse(list(self.root.glob(".download-*")))

    def test_unofficial_origin_and_redirect_refused(self):
        with self.assertRaises(runtime.PreparationError):
            runtime.download(
                self.root / "archive",
                replace(self.pin, url="http://untrusted.invalid"),
                runtime.Deadline(),
            )
        with self.assertRaises(runtime.PreparationError):
            runtime.NoRedirect().redirect_request(
                None, None, 302, "redirect", {}, self.pin.url
            )


if __name__ == "__main__":
    unittest.main()
