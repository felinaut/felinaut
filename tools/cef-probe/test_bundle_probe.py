"""Offline file-fixture tests. Mach-O header fixtures are never executed."""

import hashlib
import io
import json
import os
import plistlib
import stat
import struct
import tarfile
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path
from unittest.mock import patch

import bundle_probe as bundle
import prepare_runtime as runtime


class BundleTests(unittest.TestCase):
    def setUp(self):
        artifacts = runtime.private_directory(runtime.DEFAULT_ROOT)
        temporary = tempfile.TemporaryDirectory(prefix=".bundle-tests-", dir=artifacts)
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.runtime_root = self.base / "sdk"
        self.runtime_root.mkdir(mode=0o700)
        self.pin = runtime.Pin()
        self.dylib = self.macho(6, b"upstream signed bytes fixture")
        prefix = runtime.FRAMEWORK
        entries = [
            (
                "include/cef_version.h",
                f'#define CEF_VERSION "{self.pin.version}"\n'.encode(),
            ),
            ("LICENSE.txt", b"CEF publisher license fixture\n"),
            ("CREDITS.html", b"<html>Chromium publisher credits fixture</html>\n"),
            (prefix + "/Versions/A/Chromium Embedded Framework", self.dylib),
            (prefix + "/Versions/A/Libraries/libcef_sandbox.dylib", self.dylib),
            (prefix + "/Versions/A/Resources/icudtl.dat", b"resource fixture"),
            (prefix + "/Versions/Current", "A"),
            (
                prefix + "/Chromium Embedded Framework",
                "Versions/Current/Chromium Embedded Framework",
            ),
            (prefix + "/Libraries", "Versions/Current/Libraries"),
            (prefix + "/Resources", "Versions/Current/Resources"),
        ]
        archive_path = self.runtime_root / self.pin.name
        with tarfile.open(archive_path, "w:bz2") as archive:
            for name, value in entries:
                info = tarfile.TarInfo(self.pin.top + "/" + name)
                info.mode = (
                    0o755
                    if name.endswith("Framework") or name.endswith(".dylib")
                    else 0o644
                )
                if isinstance(value, str):
                    info.type = tarfile.SYMTYPE
                    info.linkname = value
                    archive.addfile(info)
                else:
                    info.size = len(value)
                    archive.addfile(info, io.BytesIO(value))
        data = archive_path.read_bytes()
        self.pin = replace(
            self.pin, size=len(data), sha1=hashlib.sha1(data).hexdigest()
        )
        self.sdk, _ = runtime.prepare(
            self.runtime_root,
            True,
            self.pin,
            runtime.Limits(extracted_bytes=1024 * 1024, reserve_bytes=0),
        )
        pin_patch = patch.object(bundle, "PIN", self.pin)
        pin_patch.start()
        self.addCleanup(pin_patch.stop)
        self.main = self.base / "main"
        self.helper = self.base / "helper"
        for path, value in ((self.main, b"our main"), (self.helper, b"our helper")):
            path.write_bytes(self.macho(2, value))
            path.chmod(0o755)
        self.destination = self.base / "bundles/probe.app"

    @staticmethod
    def macho(kind, payload):
        return (
            struct.pack("<IIIIIIII", 0xFEEDFACF, 0x0100000C, 0, kind, 0, 0, 0, 0)
            + payload
        )

    def create(self):
        return bundle.create_bundle(self.destination, self.sdk, self.main, self.helper)

    def snapshot(self, root):
        result = {}
        for parent, directories, files in os.walk(root, followlinks=False):
            for name in directories + files:
                path = Path(parent) / name
                info = path.lstat()
                value = (
                    os.readlink(path)
                    if path.is_symlink()
                    else (
                        hashlib.sha256(path.read_bytes()).hexdigest()
                        if path.is_file()
                        else None
                    )
                )
                result[path.relative_to(root).as_posix()] = (
                    stat.S_IMODE(info.st_mode),
                    info.st_mtime_ns,
                    value,
                )
        return result

    def test_complete_bundle_preserves_sources_framework_and_loader_paths(self):
        before = self.snapshot(self.runtime_root)
        original_main, original_helper = (
            self.main.read_bytes(),
            self.helper.read_bytes(),
        )
        with patch(
            "urllib.request.build_opener",
            side_effect=AssertionError("network forbidden"),
        ):
            result = self.create()
        self.assertEqual(result, self.destination)
        self.assertEqual(result.stat().st_mode & 0o777, 0o700)
        main_exe = result / "Contents/MacOS" / bundle.EXECUTABLE
        self.assertEqual(main_exe.read_bytes(), original_main)
        framework = result / "Contents/Frameworks" / bundle.FRAMEWORK
        source = self.sdk / runtime.FRAMEWORK
        for parent, directories, files in os.walk(source, followlinks=False):
            for leaf in directories + files:
                path = Path(parent) / leaf
                copied = framework / path.relative_to(source)
                self.assertEqual(
                    copied.lstat().st_mode & 0o777, path.lstat().st_mode & 0o777
                )
                if path.is_symlink():
                    self.assertTrue(copied.is_symlink())
                    self.assertEqual(os.readlink(copied), os.readlink(path))
                elif path.is_file():
                    self.assertEqual(copied.read_bytes(), path.read_bytes())
        main_loader = (
            main_exe.parent
            / "../Frameworks"
            / bundle.FRAMEWORK
            / "Chromium Embedded Framework"
        )
        self.assertEqual(
            main_loader.resolve(),
            (framework / "Versions/A/Chromium Embedded Framework").resolve(),
        )
        identifiers = set()
        with (result / "Contents/Info.plist").open("rb") as source:
            plist = plistlib.load(source)
        self.assertEqual(plist["CFBundleExecutable"], bundle.EXECUTABLE)
        self.assertNotIn("LSEnvironment", plist)
        for suffix, _ in bundle.HELPERS:
            name = bundle.EXECUTABLE + " " + suffix
            helper_app = result / "Contents/Frameworks" / (name + ".app")
            helper_exe = helper_app / "Contents/MacOS" / name
            self.assertEqual(helper_exe.read_bytes(), original_helper)
            self.assertEqual(helper_exe.stat().st_mode & 0o777, 0o755)
            with (helper_app / "Contents/Info.plist").open("rb") as source:
                plist = plistlib.load(source)
            identifiers.add(plist["CFBundleIdentifier"])
            self.assertEqual(plist["CFBundleExecutable"], name)
            self.assertEqual(plist["CFBundleName"], name)
            self.assertTrue(plist["LSUIElement"])
            self.assertNotIn("LSEnvironment", plist)
            for relative in (
                "Chromium Embedded Framework",
                "Libraries/libcef_sandbox.dylib",
            ):
                loader = helper_exe.parent / "../../.." / bundle.FRAMEWORK / relative
                self.assertEqual(
                    loader.resolve(strict=True),
                    (framework / relative).resolve(strict=True),
                )
        self.assertEqual(len(identifiers), 5)
        for name in bundle.LICENSES:
            copied = result / "Contents/Resources/cef" / name
            self.assertEqual(copied.read_bytes(), (self.sdk / name).read_bytes())
            self.assertEqual(copied.stat().st_mode & 0o777, 0o444)
        self.assertEqual(self.snapshot(self.runtime_root), before)
        self.assertEqual(self.main.read_bytes(), original_main)
        self.assertEqual(self.helper.read_bytes(), original_helper)

    def test_existing_directory_and_file_destinations_preserved(self):
        self.destination.parent.mkdir(mode=0o700)
        self.destination.mkdir()
        sentinel = self.destination / "keep"
        sentinel.write_text("unrelated")
        with self.assertRaisesRegex(bundle.BundleError, "exists"):
            self.create()
        self.assertEqual(sentinel.read_text(), "unrelated")
        sentinel.unlink()
        self.destination.rmdir()
        self.destination.write_text("unrelated file")
        with self.assertRaises(bundle.BundleError):
            self.create()
        self.assertEqual(self.destination.read_text(), "unrelated file")

    def test_symlink_and_broken_symlink_destination_preserved(self):
        self.destination.parent.mkdir(mode=0o700)
        for target in (self.base, self.base / "missing"):
            with self.subTest(target=target):
                self.destination.symlink_to(target)
                with self.assertRaisesRegex(bundle.BundleError, "Symlink"):
                    self.create()
                self.assertTrue(self.destination.is_symlink())
                self.destination.unlink()

    def test_symlink_parent_escape_refused(self):
        self.destination.parent.symlink_to(self.base, target_is_directory=True)
        with self.assertRaisesRegex(bundle.BundleError, "Symlink"):
            self.create()
        self.assertFalse((self.base / "probe.app").exists())

    def test_outside_target_and_traversal_and_invalid_suffix(self):
        for destination in (
            Path("/tmp/felinaut-probe-disallowed.app"),
            self.base / "../escape.app",
            self.base / "not-app",
            self.base / ".app",
        ):
            with self.subTest(destination=destination):
                with self.assertRaises(bundle.BundleError):
                    bundle.create_bundle(destination, self.sdk, self.main, self.helper)

    def test_nonprivate_parent_is_not_chmodded(self):
        self.destination.parent.mkdir(mode=0o755)
        with self.assertRaisesRegex(runtime.PreparationError, "private"):
            self.create()
        self.assertEqual(self.destination.parent.stat().st_mode & 0o777, 0o755)
        self.assertFalse(self.destination.exists())

    def test_missing_main_or_helper(self):
        for path in (self.main, self.helper):
            with self.subTest(path=path):
                saved = path.read_bytes()
                path.unlink()
                with self.assertRaises(OSError):
                    self.create()
                self.assertFalse(self.destination.exists())
                path.write_bytes(saved)
                path.chmod(0o755)

    def test_wrong_executable_formats_and_truncation(self):
        valid = self.main.read_bytes()
        x64 = (
            struct.pack("<IIIIIIII", 0xFEEDFACF, 0x01000007, 3, 2, 0, 0, 0, 0) + b"x64"
        )
        for data in (
            b"",
            valid[:32],
            b"#!script fixture",
            x64,
            self.dylib,
            b"\xca\xfe\xba\xbe" + valid,
        ):
            with self.subTest(data=data[:16]):
                self.main.write_bytes(data)
                with self.assertRaises(bundle.BundleError):
                    self.create()
                self.assertFalse(self.destination.exists())

    def test_helper_wrong_architecture_is_also_checked(self):
        self.helper.write_bytes(b"not a Mach-O" * 8)
        with self.assertRaisesRegex(bundle.BundleError, "arm64"):
            self.create()
        self.assertFalse(self.destination.exists())

    def test_executable_modes_symlinks_and_size_cap(self):
        for mode in (0o644, 0o4755):
            self.helper.chmod(mode)
            with self.assertRaisesRegex(bundle.BundleError, "permission"):
                self.create()
        self.helper.chmod(0o755)
        self.helper.unlink()
        self.helper.symlink_to(self.main)
        with self.assertRaisesRegex(bundle.BundleError, "Symlink"):
            self.create()
        self.helper.unlink()
        self.helper.write_bytes(self.main.read_bytes())
        self.helper.chmod(0o755)
        with patch.object(bundle, "MAX_BINARY_BYTES", 33):
            with self.assertRaisesRegex(bundle.BundleError, "size cap"):
                self.create()

    def test_same_executable_for_both_roles_refused(self):
        with self.assertRaisesRegex(bundle.BundleError, "distinct"):
            bundle.create_bundle(self.destination, self.sdk, self.main, self.main)

    def test_sdk_archive_checksum_and_manifest_required(self):
        archive = self.runtime_root / self.pin.name
        data = archive.read_bytes()
        archive.write_bytes(b"x" * len(data))
        with self.assertRaisesRegex(runtime.PreparationError, "checksum"):
            self.create()
        archive.write_bytes(data)
        (self.sdk / runtime.MARKER).unlink()
        with self.assertRaises(OSError):
            self.create()
        self.assertFalse(self.destination.exists())

    def test_modified_sdk_refused_and_not_repaired(self):
        header = self.sdk / "include/cef_version.h"
        header.write_text('#define CEF_VERSION "older"\n')
        before = self.snapshot(self.runtime_root)
        with self.assertRaisesRegex(runtime.PreparationError, "file changed"):
            self.create()
        self.assertEqual(self.snapshot(self.runtime_root), before)
        self.assertFalse(self.destination.exists())

    def test_exact_sdk_directory_and_no_symlink_input(self):
        for sdk in (self.sdk.parent, self.base / "156.0.25/macosarm64"):
            with self.subTest(sdk=sdk):
                with self.assertRaises(bundle.BundleError):
                    bundle.create_bundle(self.destination, sdk, self.main, self.helper)
        link = self.base / "sdk-link"
        link.symlink_to(self.runtime_root)
        with self.assertRaisesRegex(bundle.BundleError, "Symlink"):
            bundle.create_bundle(
                self.destination, link / "156.0.26/macosarm64", self.main, self.helper
            )

    def test_missing_publisher_notice_refused(self):
        (self.sdk / "LICENSE.txt").unlink()
        with self.assertRaises(runtime.PreparationError):
            self.create()
        self.assertFalse(self.destination.exists())

    def test_destination_inside_sdk_refused_without_mutating_sdk(self):
        before = self.snapshot(self.runtime_root)
        with self.assertRaisesRegex(bundle.BundleError, "prepared SDK"):
            bundle.create_bundle(
                self.sdk / "probe.app", self.sdk, self.main, self.helper
            )
        self.assertEqual(self.snapshot(self.runtime_root), before)

    def test_framework_symlink_to_other_sdk_content_is_not_copied(self):
        name = runtime.FRAMEWORK + "/Resources"
        link = self.sdk / name
        link.unlink()
        link.symlink_to("../../LICENSE.txt")
        marker_path = self.sdk / runtime.MARKER
        marker = json.loads(marker_path.read_text())
        marker["manifest"][name]["target"] = "../../LICENSE.txt"
        marker_path.write_text(json.dumps(marker))
        before = self.snapshot(self.runtime_root)
        with self.assertRaisesRegex(bundle.BundleError, "escapes the framework"):
            self.create()
        self.assertFalse(self.destination.exists())
        self.assertEqual(self.snapshot(self.runtime_root), before)

    def test_disk_budget_refused_without_output(self):
        with patch("shutil.disk_usage", return_value=type("Usage", (), {"free": 0})()):
            with self.assertRaisesRegex(bundle.BundleError, "disk budget"):
                self.create()
        self.assertFalse(self.destination.exists())

    def test_copy_failure_removes_only_our_incomplete_bundle(self):
        before = self.snapshot(self.runtime_root)
        with patch.object(
            bundle, "copy_framework", side_effect=OSError("fixture copy error")
        ):
            with self.assertRaisesRegex(OSError, "fixture copy error"):
                self.create()
        self.assertFalse(self.destination.exists())
        self.assertEqual(self.snapshot(self.runtime_root), before)

    def test_source_changes_during_copy_are_detected(self):
        original = bundle.copy_verified_file

        def changed(source, destination, entry, deadline, mode=None):
            if source == self.helper:
                source.write_bytes(source.read_bytes() + b"changed")
            return original(source, destination, entry, deadline, mode)

        with patch.object(bundle, "copy_verified_file", side_effect=changed):
            with self.assertRaisesRegex(bundle.BundleError, "grew"):
                self.create()
        self.assertFalse(self.destination.exists())

    def test_destination_appearing_after_preflight_is_preserved(self):
        original = runtime.private_directory

        def appeared(path):
            result = original(path)
            if path == self.destination.parent:
                self.destination.mkdir(mode=0o700)
                (self.destination / "keep").write_text("another caller")
            return result

        with patch.object(runtime, "private_directory", side_effect=appeared):
            with self.assertRaises(FileExistsError):
                self.create()
        self.assertEqual((self.destination / "keep").read_text(), "another caller")


if __name__ == "__main__":
    unittest.main()
