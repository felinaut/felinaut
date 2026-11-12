"""Packaging metadata/layout checks; no release build or Finder access required."""

import hashlib
import io
import os
import plistlib
import runpy
import struct
import tempfile
import tomllib
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import package_macos as package


class PackagingTests(unittest.TestCase):
    def test_bundle_metadata_round_trips_and_supports_browser_http(self):
        info = package.info_plist("0.1.0", "0.1 beta")
        self.assertEqual(plistlib.loads(plistlib.dumps(info)), info)
        self.assertEqual(info["CFBundleName"], "Felinaut")
        self.assertEqual(info["CFBundleDisplayName"], "Felinaut")
        self.assertEqual(info["CFBundleExecutable"], "felinaut")
        self.assertEqual(info["CFBundleIconFile"], "Felinaut.icns")
        self.assertEqual(info["CFBundleIdentifier"], "io.github.casaroli.felifox")
        self.assertEqual(info["CFBundleShortVersionString"], "0.1.0")
        self.assertEqual(info["CFBundleGetInfoString"], "Felinaut 0.1 beta")
        self.assertTrue(
            info["NSAppTransportSecurity"]["NSAllowsArbitraryLoadsInWebContent"]
        )
        self.assertNotIn("NSAllowsArbitraryLoads", info["NSAppTransportSecurity"])
        schemes = [t["CFBundleURLSchemes"] for t in info["CFBundleURLTypes"]]
        self.assertEqual(schemes, [["http", "https"]])

    def test_signing_settings_prefer_canonical_names_with_legacy_fallbacks(self):
        suffixes = ("SIGN_IDENTITY", "NOTARY_PROFILE", "PROVISIONING_PROFILE")
        cases = [
            ({}, ""),
            ({"FELIFOX_": "legacy"}, "legacy"),
            ({"FELINAUT_": "canonical"}, "canonical"),
            ({"FELIFOX_": "legacy", "FELINAUT_": "canonical"}, "canonical"),
            ({"FELIFOX_": "legacy", "FELINAUT_": ""}, ""),
        ]
        for prefixes, expected in cases:
            env = {
                prefix + suffix: value
                for prefix, value in prefixes.items()
                for suffix in suffixes
            }
            with self.subTest(env=env), patch.dict(os.environ, env, clear=True):
                settings = runpy.run_path(package.__file__)
                for suffix in suffixes:
                    self.assertEqual(settings[suffix], expected)
        # Settings may also mix old and new names independently.
        with patch.dict(
            os.environ,
            {
                "FELINAUT_SIGN_IDENTITY": "developer",
                "FELIFOX_NOTARY_PROFILE": "notary",
                "FELIFOX_PROVISIONING_PROFILE": "profile",
            },
            clear=True,
        ):
            settings = runpy.run_path(package.__file__)
        self.assertEqual(settings["SIGN_IDENTITY"], "developer")
        self.assertEqual(settings["NOTARY_PROFILE"], "notary")
        self.assertEqual(settings["PROVISIONING_PROFILE"], "profile")

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

    def test_signing_configuration_errors_use_canonical_names(self):
        for setting in ("NOTARY_PROFILE", "PROVISIONING_PROFILE"):
            settings = {
                "SIGN_IDENTITY": "",
                "NOTARY_PROFILE": "",
                "PROVISIONING_PROFILE": "",
            }
            settings[setting] = "configured"
            with (
                self.subTest(setting=setting),
                patch.multiple(package, **settings),
                patch.object(package.platform, "system", return_value="Darwin"),
                patch.object(package.sys, "argv", ["package_macos.py"]),
                patch.object(package.sys, "stderr", new_callable=io.StringIO) as errors,
                self.assertRaises(SystemExit) as error,
            ):
                package.main()
            self.assertEqual(error.exception.code, 2)
            self.assertIn(
                f"FELINAUT_{setting} requires FELINAUT_SIGN_IDENTITY", errors.getvalue()
            )

    def test_provisioned_app_embeds_profile_and_renamed_entitlements(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "felinaut"
            binary.write_bytes(b"release executable")
            icon = root / "Felinaut.icns"
            icon.write_bytes(b"icon")
            profile = root / "browser.provisionprofile"
            profile.write_bytes(b"provisioning profile")
            destination = root / "Felinaut.app"
            with (
                patch.object(package, "PROVISIONING_PROFILE", str(profile)),
                patch.object(
                    package, "SIGN_IDENTITY", "Developer ID Application: X (T)"
                ),
                patch.object(package, "run") as commands,
            ):
                package.make_app(binary, icon, destination, "0.1.0", "0.1 beta")
            contents = destination / "Contents"
            self.assertEqual(
                (contents / "embedded.provisionprofile").read_bytes(),
                profile.read_bytes(),
            )
            info = plistlib.loads((contents / "Info.plist").read_bytes())
            self.assertEqual(info["CFBundleIdentifier"], "io.github.casaroli.felifox")
            self.assertEqual(package.ENTITLEMENTS.name, "Felinaut.entitlements")
            sign = next(
                c
                for c in commands.call_args_list
                if c.args[:2] == ("codesign", "--force")
            )
            self.assertEqual(
                sign.args[sign.args.index("--entitlements") + 1],
                str(package.ENTITLEMENTS),
            )
            self.assertEqual(sign.args[-1], destination)
            self.assertIn("runtime", sign.args)
            self.assertIn("--timestamp", sign.args)

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
            self.assertEqual(package.available_volume_name(volumes), "Felinaut")
            (volumes / "Felinaut").mkdir()
            self.assertEqual(
                package.available_volume_name(volumes), "Felinaut Installer"
            )
            (volumes / "Felinaut Installer").mkdir()
            self.assertEqual(
                package.available_volume_name(volumes), "Felinaut Installer 2"
            )
            self.assertTrue((volumes / "Felinaut").is_dir())

    def test_cargo_metadata_names_match_bundle_executable(self):
        cargo = tomllib.loads((package.ROOT / "Cargo.toml").read_text())
        lock = tomllib.loads((package.ROOT / "Cargo.lock").read_text())
        self.assertEqual(cargo["package"]["name"], "felinaut")
        self.assertEqual(
            cargo["package"]["default-run"],
            package.info_plist("0.1.0", "beta")["CFBundleExecutable"],
        )
        self.assertIn(
            {"name": "felinaut", "version": cargo["package"]["version"]},
            [{"name": p["name"], "version": p["version"]} for p in lock["package"]],
        )
        dist_root = package.ROOT / "tools" / "dist"
        dist = tomllib.loads((dist_root / "Cargo.toml").read_text())
        dist_lock = tomllib.loads((dist_root / "Cargo.lock").read_text())
        self.assertEqual(dist["package"]["name"], "felinaut-dist")
        self.assertEqual(
            dist_lock["package"],
            [{"name": "felinaut-dist", "version": dist["package"]["version"]}],
        )

    def test_packaging_builds_and_installs_the_renamed_executable(self):
        for host, arch in [
            ("aarch64-apple-darwin", "arm64"),
            ("x86_64-apple-darwin", "x86_64"),
        ]:
            for app_only in (True, False):
                with (
                    self.subTest(host=host, app_only=app_only),
                    tempfile.TemporaryDirectory() as directory,
                ):
                    root = Path(directory)
                    (root / "src").mkdir()
                    (root / "Cargo.toml").write_text('[package]\nversion = "0.1.0"\n')
                    (root / "src" / "main.rs").write_text(
                        'pub const DISPLAY_VERSION: &str = "0.1 beta";'
                    )
                    (root / "LICENSE").write_text("test license")
                    target = root / "target"
                    binary = target / host / "release" / "felinaut"
                    icon = root / "Felinaut.icns"
                    icon.write_bytes(b"icon")

                    def run(*args, **kwargs):
                        if args[:2] == ("rustc", "-vV"):
                            return SimpleNamespace(stdout=f"host: {host}\n")
                        if args[:2] == ("cargo", "metadata"):
                            return SimpleNamespace(
                                stdout=package.json.dumps(
                                    {"target_directory": str(target)}
                                )
                            )
                        if args[:2] == ("cargo", "build"):
                            binary.parent.mkdir(parents=True)
                            binary.write_bytes(b"fresh release executable")
                        return SimpleNamespace(stdout="")

                    def make_dmg(app, art, icon, work, name):
                        self.assertEqual(app.name, "Felinaut.app")
                        output = work / name
                        output.write_bytes(b"test disk image")
                        return output

                    argv = ["package_macos.py"] + (["--app-only"] if app_only else [])
                    with (
                        patch.object(package, "ROOT", root),
                        patch.object(package, "SIGN_IDENTITY", ""),
                        patch.object(package, "NOTARY_PROFILE", ""),
                        patch.object(package, "PROVISIONING_PROFILE", ""),
                        patch.object(package.platform, "system", return_value="Darwin"),
                        patch.object(package.shutil, "which", return_value="/tool"),
                        patch.object(package.sys, "argv", argv),
                        patch.object(package, "run", side_effect=run) as commands,
                        patch.object(package, "make_icon", return_value=icon),
                        patch.object(package, "make_dmg", side_effect=make_dmg),
                    ):
                        package.main()
                    contents = root / "dist" / "Felinaut.app" / "Contents"
                    info = plistlib.loads((contents / "Info.plist").read_bytes())
                    self.assertEqual(
                        (contents / "MacOS" / info["CFBundleExecutable"]).read_bytes(),
                        b"fresh release executable",
                    )
                    self.assertEqual(
                        (
                            contents / "Resources" / info["CFBundleIconFile"]
                        ).read_bytes(),
                        b"icon",
                    )
                    self.assertFalse((contents / "embedded.provisionprofile").exists())
                    build = next(
                        c
                        for c in commands.call_args_list
                        if c.args[:2] == ("cargo", "build")
                    )
                    self.assertIn("--locked", build.args)
                    self.assertEqual(
                        build.args[build.args.index("--bin") + 1], "felinaut"
                    )
                    self.assertEqual(build.args[build.args.index("--target") + 1], host)
                    render = next(
                        c for c in commands.call_args_list if c.args[0] == "swift"
                    )
                    self.assertEqual(
                        render.args[2:4],
                        (
                            root / "assets/felinaut-mascot.svg",
                            root / "assets/felinaut-symbol.svg",
                        ),
                    )
                    if not app_only:
                        name = f"Felinaut-0.1-beta-macos-{arch}.dmg"
                        self.assertEqual(
                            (root / "dist" / name).read_bytes(), b"test disk image"
                        )
                        self.assertEqual(
                            (root / "dist" / "SHA256SUMS").read_text(),
                            f"{hashlib.sha256(b'test disk image').hexdigest()}  {name}\n",
                        )

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
        self.assertIn(b"/Volumes/Felinaut", portable)
        self.assertNotIn(b"/Users/builder", portable)
        self.assertEqual(struct.unpack_from(">H", portable, 4)[0], len(portable))
        with self.assertRaises(ValueError):
            package.portable_alias(b"invalid")

    def test_finder_paths_are_quoted_and_layout_matches_art(self):
        path = Path('/tmp/Felinaut "test" volume')
        script = package.finder_script(path)
        self.assertIn('Felinaut \\"test\\" volume', script)
        self.assertIn("{190, 245}", script)
        self.assertIn("{530, 245}", script)
        self.assertIn(".background/installer.png", script)
        self.assertIn("set background picture", script)
        self.assertIn("update installerFolder without registering applications", script)
        self.assertIn("set arrangement of options to not arranged", script)


if __name__ == "__main__":
    unittest.main()
