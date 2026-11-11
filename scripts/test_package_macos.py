"""Packaging metadata/layout checks; no release build or Finder access required."""

import plistlib
import struct
import tempfile
import tomllib
import unittest
from pathlib import Path

import package_macos as package


class PackagingTests(unittest.TestCase):
    def test_bundle_metadata_round_trips_and_supports_browser_http(self):
        info = package.info_plist("0.1.0", "0.1 beta")
        self.assertEqual(plistlib.loads(plistlib.dumps(info)), info)
        self.assertEqual(info["CFBundleExecutable"], "felifox")
        self.assertEqual(info["CFBundleIconFile"], "Felifox.icns")
        self.assertEqual(info["CFBundleIdentifier"], "io.github.casaroli.felifox")
        self.assertEqual(info["CFBundleShortVersionString"], "0.1.0")
        self.assertEqual(info["CFBundleGetInfoString"], "Felifox 0.1 beta")
        self.assertTrue(
            info["NSAppTransportSecurity"]["NSAllowsArbitraryLoadsInWebContent"]
        )
        self.assertNotIn("NSAllowsArbitraryLoads", info["NSAppTransportSecurity"])
        schemes = [t["CFBundleURLSchemes"] for t in info["CFBundleURLTypes"]]
        self.assertEqual(schemes, [["http", "https"]])

    def test_developer_id_signing_uses_hardened_runtime_and_timestamp(self):
        self.assertEqual(package.codesign_args(""), ["--force", "--sign", "-"])
        args = package.codesign_args("Developer ID Application: X (T)")
        self.assertEqual(args[args.index("--options") + 1], "runtime")
        self.assertIn("--timestamp", args)
        self.assertEqual(args[-1], "Developer ID Application: X (T)")
        self.assertNotIn("--entitlements", args)

    def test_passkey_entitlement_is_claimed_only_with_a_provisioning_profile(self):
        with open(package.ENTITLEMENTS, "rb") as file:
            entitlements = plistlib.load(file)
        self.assertEqual(
            entitlements,
            {"com.apple.developer.web-browser.public-key-credential": True},
        )
        self.assertIsNone(package.app_entitlements(""))
        self.assertEqual(package.app_entitlements("/x.provisionprofile"), package.ENTITLEMENTS)
        args = package.codesign_args("Developer ID Application: X (T)", package.ENTITLEMENTS)
        self.assertEqual(args[args.index("--entitlements") + 1], str(package.ENTITLEMENTS))
        self.assertIn("runtime", args)

    def test_dist_command_rebuilds_and_replaces_previous_artifacts(self):
        makefile = (package.ROOT / "Makefile").read_text()
        self.assertIn(".PHONY: dist", makefile)
        self.assertIn("dist:\n\tcargo dist", makefile)
        self.assertNotIn("--skip-build", makefile)
        config = tomllib.loads((package.ROOT / ".cargo/config.toml").read_text())
        command = config["alias"]["dist"]
        self.assertIn("--manifest-path tools/dist/Cargo.toml", command)
        self.assertIn("--locked", command)
        self.assertFalse(command.startswith("!"))

    def test_packaging_does_not_reuse_an_already_mounted_volume_name(self):
        with tempfile.TemporaryDirectory() as directory:
            volumes = Path(directory)
            self.assertEqual(package.available_volume_name(volumes), "Felifox")
            (volumes / "Felifox").mkdir()
            self.assertEqual(
                package.available_volume_name(volumes), "Felifox Installer"
            )
            (volumes / "Felifox Installer").mkdir()
            self.assertEqual(
                package.available_volume_name(volumes), "Felifox Installer 2"
            )
            self.assertTrue((volumes / "Felifox").is_dir())

    def test_versions_come_from_source(self):
        source = (package.ROOT / "src" / "main.rs").read_text()
        self.assertEqual(package.display_version(source), "0.1 beta")
        with self.assertRaises(ValueError):
            package.display_version("no display version")
        with self.assertRaises(ValueError):
            package.info_plist("0.1-beta", "0.1 beta")

    def test_background_alias_is_portable_and_store_size_is_preserved(self):
        header = bytearray(150)
        struct.pack_into(">H", header, 6, 2)
        alias = bytes(header)
        for tag, value in [
            (18, b"/.background/installer.png"),
            (19, b"/Users/builder/tmp"),
            (20, bytes(384)),
            (21, b"\x00\x0c"),
            (-1, b""),
        ]:
            alias += struct.pack(">hH", tag, len(value)) + value + bytes(len(value) % 2)
        original = plistlib.dumps(
            {"backgroundImageAlias": alias, "iconSize": 104.0}, fmt=plistlib.FMT_BINARY
        )
        data = (
            b"test-DSStore-prefix"
            + b"icvpblob"
            + struct.pack(">I", len(original))
            + original
            + b"unmodified-suffix"
        )
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / ".DS_Store"
            path.write_bytes(data)
            package.portable_finder_background(path)
            updated = path.read_bytes()
        self.assertEqual(len(updated), len(data))
        self.assertTrue(updated.endswith(b"unmodified-suffix"))
        offset = updated.index(b"icvpblob") + 12
        settings = plistlib.loads(updated[offset : offset + len(original)])
        portable = settings["backgroundImageAlias"]
        self.assertIn(b"/.background/installer.png", portable)
        self.assertIn(b"/Volumes/Felifox", portable)
        self.assertNotIn(b"/Users/builder", portable)
        self.assertEqual(struct.unpack_from(">H", portable, 4)[0], len(portable))
        with self.assertRaises(ValueError):
            package.portable_alias(b"invalid")

    def test_finder_paths_are_quoted_and_layout_matches_art(self):
        path = Path('/tmp/Felifox "test" volume')
        script = package.finder_script(path)
        self.assertIn('Felifox \\"test\\" volume', script)
        self.assertIn("{190, 245}", script)
        self.assertIn("{530, 245}", script)
        self.assertIn(".background/installer.png", script)
        self.assertIn("set background picture", script)
        self.assertIn("update installerFolder without registering applications", script)
        self.assertIn("set arrangement of options to not arranged", script)


if __name__ == "__main__":
    unittest.main()
