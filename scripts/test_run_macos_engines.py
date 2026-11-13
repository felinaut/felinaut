"""Offline behavioral tests; tiny Mach-O fixtures are NEVER executed.

No real SDK, Cargo build, desktop, installed app or normal HOME is used.
The fixture server tests do use a real ephemeral IPv4 loopback listener.
"""

import hashlib
import http.client
import io
import json
import os
import plistlib
import re
import shutil
import socket
import stat
import struct
import subprocess
import tarfile
import tempfile
import threading
import unittest
from contextlib import contextmanager, redirect_stdout
from dataclasses import replace
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

import run_macos_engines as engines


def engine_manifest(cef=True):
    return {
        "schema_version": 1,
        "engines": [
            {
                "id": identifier,
                "feature": feature,
                "family": family,
                "built": built,
                "adapter_implemented": True,
                "requires_isolated_launcher": False,
                "limitation": "Offline metadata fixture; never executed",
            }
            for identifier, feature, family, built in (
                ("system-webkit", "engine-webkit-system", "webkit", True),
                ("cef", "engine-cef", "chromium", cef),
                ("upstream-webkit", "engine-webkit-upstream", "webkit", False),
            )
        ],
    }


class PlanTests(unittest.TestCase):
    def test_default_plan_has_no_side_effects(self):
        with (
            patch.object(
                engines, "probe_tools", side_effect=AssertionError("SDK access")
            ),
            patch.object(
                engines.subprocess, "run", side_effect=AssertionError("command")
            ),
            patch.object(
                engines.subprocess, "Popen", side_effect=AssertionError("launch")
            ),
            patch.object(
                engines.tempfile, "mkdtemp", side_effect=AssertionError("write")
            ),
            redirect_stdout(io.StringIO()) as output,
        ):
            self.assertEqual(engines.main([]), 0)
        plan = json.loads(output.getvalue())
        self.assertEqual(plan["engines"], ["webkit", "cef"])
        self.assertFalse(plan["launch"])
        self.assertFalse(plan["sdk_acquisition"])
        self.assertEqual(plan["root_executable"], "Contents/MacOS/felinaut")

    def test_webkit_plan_omits_sdk_and_helper(self):
        with redirect_stdout(io.StringIO()) as output:
            engines.main(["--webkit-only", "--runtime-shaders"])
        plan = json.loads(output.getvalue())
        self.assertIsNone(plan["sdk"])
        self.assertIsNone(plan["helper_executable"])
        self.assertEqual(len(plan["build_commands"]), 1)
        self.assertIn("gpui/runtime_shaders", plan["build_commands"][0])

    def test_invalid_modes_and_timeouts_fail_before_actions(self):
        for args in (
            ["--run", "--smoke"],
            ["--smoke", "--webkit-only"],
            ["--capture"],
            ["--build-timeout", "0"],
            ["--build-timeout", "1801"],
            ["--smoke-timeout", "121"],
            ["--smoke-timeout", "nan"],
            ["--sdk", "/other"],
            ["--skip-build"],
        ):
            with (
                self.subTest(args=args),
                redirect_stdout(io.StringIO()),
                patch("sys.stderr", io.StringIO()),
            ):
                with self.assertRaises(SystemExit) as error:
                    engines.main(args)
                self.assertEqual(error.exception.code, 2)

    def test_build_commands_target_actual_root_and_production_helper(self):
        main, helper = engines.build_commands()
        self.assertEqual(
            main[:7],
            [
                "cargo",
                "build",
                "--locked",
                "--features",
                "engine-cef",
                "--bins",
                "--target-dir",
            ],
        )
        self.assertEqual(
            helper,
            [
                "cargo",
                "build",
                "--locked",
                "--manifest-path",
                str(engines.HELPER_MANIFEST),
                "--bin",
                "felinaut-helper",
                "--target-dir",
                str(engines.TARGET),
            ],
        )
        self.assertEqual(
            engines.build_commands(False),
            [
                [
                    "cargo",
                    "build",
                    "--locked",
                    "--bins",
                    "--target-dir",
                    str(engines.TARGET),
                ],
            ],
        )

    def test_build_environment_has_only_exact_cef_path(self):
        inherited = {
            "PATH": "/tools",
            "RUSTUP_TOOLCHAIN": "stable",
            "CEF_PATH": "/evil",
            "CEF_FLAGS": "--no-sandbox",
            "DYLD_FRAMEWORK_PATH": "/evil",
            "__XPC_DYLD_LIBRARY_PATH": "/evil",
            "NIX_CEF_BINARY": "/evil",
            "FELINAUT_NATIVE_TEST": "1",
            "FELIFOX_AGENT": "1",
            "WEBKIT_FRAMEWORK_PATH": "/evil",
            "CHROME_FLAGS": "--no-sandbox",
            "CARGO_TARGET_DIR": "/elsewhere",
            "CARGO_BUILD_TARGET": "other",
        }
        env = engines.build_environment(inherited)
        self.assertEqual(
            env,
            {
                "PATH": "/tools",
                "RUSTUP_TOOLCHAIN": "stable",
                "CEF_PATH": str(engines.SDK),
            },
        )
        self.assertNotIn("CEF_PATH", engines.build_environment(inherited, False))
        self.assertEqual(inherited["CEF_PATH"], "/evil")

    def test_tab_state_handles_empty_malformed_and_loading_responses(self):
        url = "http://127.0.0.1:1234/"
        for tabs in (None, {}, [], [None], [1], ["text"], [{}], [{"url": url}] * 2):
            with self.subTest(tabs=tabs):
                self.assertFalse(engines.single_tab_at(tabs, url))
        self.assertTrue(engines.single_tab_at([{"url": url}], url))
        for loading in (None, True, "false", 0):
            self.assertFalse(
                engines.single_tab_at(
                    [{"url": url, "loading": loading}], url, loaded=True
                )
            )
        self.assertTrue(
            engines.single_tab_at([{"url": url, "loading": False}], url, loaded=True)
        )

    def test_command_timeout_is_bounded_and_actionable(self):
        with patch.object(
            engines.subprocess, "run", side_effect=subprocess.TimeoutExpired("cargo", 7)
        ) as run:
            with self.assertRaisesRegex(engines.EngineError, "timed out after 7s"):
                engines.run_command(["cargo", "build"], env={}, timeout=7)
        self.assertEqual(run.call_args.kwargs["timeout"], 7)
        self.assertEqual(run.call_args.kwargs["cwd"], engines.ROOT)

    def test_wrong_native_platform_refuses_before_sdk_or_build(self):
        with (
            patch.object(engines.platform, "system", return_value="Linux"),
            patch.object(engines, "probe_tools", side_effect=AssertionError("SDK")),
            patch("sys.stderr", io.StringIO()),
        ):
            with self.assertRaises(SystemExit) as error:
                engines.main(["--build"])
        self.assertEqual(error.exception.code, 1)


class PrivateFixture(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(
            dir=Path(tempfile.gettempdir()).resolve()
        )
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)

    def patch_value(self, module, name, value):
        change = patch.object(module, name, value)
        change.start()
        self.addCleanup(change.stop)


class EngineManifestTests(PrivateFixture):
    @staticmethod
    def encode(value):
        return json.dumps(value).encode()

    def test_exact_mixed_and_webkit_only_enabled_sets_accept_real_schema(self):
        for cef in (True, False):
            with self.subTest(cef=cef):
                manifest = engine_manifest(cef)
                self.assertEqual(
                    engines.parse_engine_manifest(self.encode(manifest), cef), manifest
                )
                manifest["engines"].reverse()
                self.assertEqual(
                    engines.parse_engine_manifest(self.encode(manifest), cef), manifest
                )

    def test_safe_manifest_projection_does_not_store_unknown_metadata(self):
        manifest = engine_manifest()
        manifest["unexpected"] = {"private": "not retained"}
        manifest["engines"][0]["unknown"] = "not retained"
        self.assertEqual(
            engines.parse_engine_manifest(self.encode(manifest)), engine_manifest()
        )
        self.assertIn("unexpected", manifest)

    def test_empty_oversized_non_utf8_malformed_and_nonstandard_json_rejected(self):
        data = self.encode(engine_manifest())
        exact_limit = data + b" " * (engines.MAX_ENGINE_MANIFEST_BYTES - len(data))
        self.assertEqual(engines.parse_engine_manifest(exact_limit), engine_manifest())
        for value in (
            b"",
            b" ",
            b"not JSON",
            b"\xff",
            data + b"trailing",
            exact_limit + b" ",
            b'{"schema_version":NaN}',
            b'{"value":Infinity}',
            b"[" * 2000 + b"]" * 2000,
        ):
            with self.subTest(size=len(value)):
                with self.assertRaises(engines.EngineError):
                    engines.parse_engine_manifest(value)
        with self.assertRaises(engines.EngineError):
            engines.parse_engine_manifest(data.decode())

    def test_schema_and_engine_list_have_strict_required_types(self):
        for schema in (None, True, "1", 1.0, 0, 2):
            manifest = engine_manifest()
            manifest["schema_version"] = schema
            with self.subTest(schema=schema), self.assertRaises(engines.EngineError):
                engines.parse_engine_manifest(self.encode(manifest))
        for value in (None, [], {}, {"schema_version": 1}, {"engines": []}):
            with self.subTest(value=value), self.assertRaises(engines.EngineError):
                engines.parse_engine_manifest(self.encode(value))
        for rows in (
            None,
            {},
            "engines",
            [],
            [None],
            engine_manifest()["engines"] * 22,
        ):
            manifest = {"schema_version": 1, "engines": rows}
            with (
                self.subTest(rows_type=type(rows)),
                self.assertRaises(engines.EngineError),
            ):
                engines.parse_engine_manifest(self.encode(manifest))

    def test_missing_fields_and_wrong_engine_metadata_types_are_rejected(self):
        row = engine_manifest()["engines"][0]
        for field in row:
            manifest = engine_manifest()
            del manifest["engines"][0][field]
            with self.subTest(missing=field), self.assertRaises(engines.EngineError):
                engines.parse_engine_manifest(self.encode(manifest))
        invalid_fields = {
            "id": (None, 1, [], "", "cef\n", "System-WebKit", "a" * 65),
            "built": (None, 0, 1, "true", []),
            "adapter_implemented": (None, 1, "true"),
            "requires_isolated_launcher": (None, 0, "false"),
            "feature": (None, False, "", "x" * 4097),
            "family": (None, [], "webkit\x00"),
            "limitation": (None, {}, "x" * 4097),
        }
        for field, values in invalid_fields.items():
            for value in values:
                manifest = engine_manifest()
                manifest["engines"][0][field] = value
                with (
                    self.subTest(field=field, value_type=type(value)),
                    self.assertRaises(engines.EngineError),
                ):
                    engines.parse_engine_manifest(self.encode(manifest))

    def test_duplicate_engine_ids_and_duplicate_json_keys_are_rejected(self):
        manifest = engine_manifest()
        manifest["engines"].append(dict(manifest["engines"][-1]))
        with self.assertRaisesRegex(engines.EngineError, "duplicate engine ids"):
            engines.parse_engine_manifest(self.encode(manifest))
        raw = self.encode(engine_manifest())
        for value in (
            raw.replace(
                b'"schema_version": 1', b'"schema_version": 1, "schema_version": 1', 1
            ),
            raw.replace(b'"built": true', b'"built": false, "built": true', 1),
        ):
            with self.assertRaisesRegex(engines.EngineError, "duplicate JSON fields"):
                engines.parse_engine_manifest(value)

    def test_missing_extra_and_wrongly_enabled_engines_cannot_pass(self):
        cases = []
        missing_cef = engine_manifest(False)
        cases.append((missing_cef, True))
        cases.append((engine_manifest(True), False))
        extra = engine_manifest()
        extra["engines"][-1]["built"] = True
        cases.append((extra, True))
        for identifier in ("cef", "system-webkit"):
            missing = engine_manifest()
            missing["engines"] = [
                row for row in missing["engines"] if row["id"] != identifier
            ]
            cases.append((missing, True))
        for manifest, cef in cases:
            with (
                self.subTest(cef=cef),
                self.assertRaisesRegex(engines.EngineError, "enabled set mismatch"),
            ):
                engines.parse_engine_manifest(self.encode(manifest), cef)

    def test_verification_uses_only_copied_executable_with_bounded_sanitized_command(
        self,
    ):
        app = self.base / "Felinaut Engines.app"
        manifest = engine_manifest()

        def emit(command, *, env, timeout, log):
            self.assertEqual(
                command, [app / "Contents/MacOS/felinaut", "--list-engines"]
            )
            self.assertEqual(timeout, 10)
            self.assertEqual(env, engines.system_environment(os.environ))
            for variable in (
                "HOME",
                "CFFIXED_USER_HOME",
                "CEF_PATH",
                "FELINAUT_AGENT",
                "FELINAUT_NATIVE_TEST",
                "DYLD_LIBRARY_PATH",
            ):
                self.assertNotIn(variable, env)
            log.write(self.encode(manifest))
            log.flush()

        with patch.object(engines, "run_command", side_effect=emit) as run:
            self.assertEqual(engines.verify_app_engines(app), manifest)
        run.assert_called_once()
        self.assertEqual(list(self.base.iterdir()), [])

    def test_verification_failure_oversize_timeout_and_exit_errors_are_actionable(self):
        app = self.base / "Felinaut Engines.app"
        for data in (
            b"",
            b"invalid JSON",
            b"x" * (engines.MAX_ENGINE_MANIFEST_BYTES + 1),
            self.encode(engine_manifest(False)),
        ):

            def emit(command, *, env, timeout, log):
                log.write(data)
                log.flush()

            with patch.object(engines, "run_command", side_effect=emit):
                with self.assertRaisesRegex(
                    engines.EngineError, "concurrent Cargo artifact overwrite"
                ) as caught:
                    engines.verify_app_engines(app)
            self.assertIn("No app launched", str(caught.exception))
            self.assertIn("sequentially", str(caught.exception))
        for failure in (
            engines.EngineError("command timed out after 10s"),
            engines.CommandError("app", 2, "private diagnostic"),
            OSError("cannot execute"),
        ):
            with patch.object(engines, "run_command", side_effect=failure):
                with self.assertRaisesRegex(
                    engines.EngineError, "engine verification refused"
                ) as caught:
                    engines.verify_app_engines(app)
            self.assertNotIn("private diagnostic", str(caught.exception))
        self.assertEqual(list(self.base.iterdir()), [])


class AssemblyTests(PrivateFixture):
    @classmethod
    def setUpClass(cls):
        cls.runtime, cls.bundle = engines.probe_tools()

    @staticmethod
    def macho(kind, payload):
        return (
            struct.pack("<IIIIIIII", 0xFEEDFACF, 0x0100000C, 0, kind, 0, 0, 0, 0)
            + payload
        )

    def setUp(self):
        super().setUp()
        target = self.base / "target"
        target.mkdir(mode=0o700)
        self.patch_value(engines, "ROOT", self.base)
        self.patch_value(engines, "TARGET", target)
        self.patch_value(engines, "SDK", target / "cef-runtime/156.0.26/macosarm64")
        self.patch_value(engines, "MAIN", target / "debug/felinaut")
        self.patch_value(engines, "HELPER", target / "debug/felinaut-helper")
        self.patch_value(
            engines, "HELPER_MANIFEST", self.base / "tools/cef-helper/Cargo.toml"
        )
        self.patch_value(engines, "TEMP_ROOT", self.base)
        engines.HELPER_MANIFEST.parent.mkdir(parents=True)
        engines.HELPER_MANIFEST.write_text("# parent helper manifest fixture\n")
        engines.MAIN.parent.mkdir()
        for path in (engines.MAIN, engines.HELPER):
            path.write_bytes(self.macho(2, path.name.encode()))
            path.chmod(0o755)
        self.output = target / "felinaut-engines-fixture"
        self.output.mkdir(mode=0o700)
        self.make_sdk()
        self.patch_value(self.bundle, "PIN", self.pin)

    def make_sdk(self):
        runtime_root = engines.TARGET / "cef-runtime"
        runtime_root.mkdir(mode=0o700)
        pin = self.runtime.Pin()
        prefix = self.runtime.FRAMEWORK
        dylib = self.macho(6, b"publisher framework bytes including signature fixture")
        entries = [
            (
                "include/cef_version.h",
                f'#define CEF_VERSION "{pin.version}"\n'.encode(),
            ),
            ("LICENSE.txt", b"CEF publisher license fixture\n"),
            ("CREDITS.html", b"Chromium credits fixture\n"),
            (prefix + "/Versions/A/Chromium Embedded Framework", dylib),
            (prefix + "/Versions/A/Libraries/libcef_sandbox.dylib", dylib),
            (prefix + "/Versions/A/Resources/icudtl.dat", b"icu resource"),
            (
                prefix + "/Versions/A/_CodeSignature/CodeResources",
                b"unchanged signature",
            ),
            (prefix + "/Versions/Current", "A"),
            (
                prefix + "/Chromium Embedded Framework",
                "Versions/Current/Chromium Embedded Framework",
            ),
            (prefix + "/Libraries", "Versions/Current/Libraries"),
            (prefix + "/Resources", "Versions/Current/Resources"),
        ]
        archive_path = runtime_root / pin.name
        with tarfile.open(archive_path, "w:bz2") as archive:
            for name, data in entries:
                info = tarfile.TarInfo(pin.top + "/" + name)
                info.mode = 0o755 if name.endswith(("Framework", ".dylib")) else 0o644
                if isinstance(data, str):
                    info.type, info.linkname = tarfile.SYMTYPE, data
                    archive.addfile(info)
                else:
                    info.size = len(data)
                    archive.addfile(info, io.BytesIO(data))
        data = archive_path.read_bytes()
        self.pin = replace(pin, size=len(data), sha1=hashlib.sha1(data).hexdigest())
        sdk, _ = self.runtime.prepare(
            runtime_root,
            True,
            self.pin,
            self.runtime.Limits(extracted_bytes=1024 * 1024, reserve_bytes=0),
        )
        self.assertEqual(sdk, engines.SDK)

    def assemble(self, cef=True):
        return engines.assemble(self.output, self.runtime, self.bundle, cef)

    @staticmethod
    def tree(root):
        result = {}
        for parent, directories, files in os.walk(root, followlinks=False):
            for name in directories + files:
                path = Path(parent) / name
                info = path.lstat()
                content = (
                    os.readlink(path)
                    if path.is_symlink()
                    else path.read_bytes()
                    if path.is_file()
                    else None
                )
                result[str(path.relative_to(root))] = (
                    stat.S_IMODE(info.st_mode),
                    content,
                )
        return result

    def test_actual_bundle_layout_and_byte_exact_framework_without_sdk_writes(self):
        before = self.tree(engines.SDK.parent.parent)
        with patch(
            "urllib.request.build_opener",
            side_effect=AssertionError("network forbidden"),
        ):
            app = self.assemble()
        self.assertEqual(before, self.tree(engines.SDK.parent.parent))
        self.assertEqual(
            (app / "Contents/MacOS/felinaut").read_bytes(), engines.MAIN.read_bytes()
        )
        info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
        self.assertEqual(info["CFBundleExecutable"], "felinaut")
        self.assertNotIn("cef-probe", info["CFBundleIdentifier"])
        framework = app / "Contents/Frameworks" / self.bundle.FRAMEWORK
        self.assertEqual(
            self.tree(engines.SDK / "Release" / self.bundle.FRAMEWORK),
            self.tree(framework),
        )
        helpers = sorted((app / "Contents/Frameworks").glob("*.app"))
        self.assertEqual(len(helpers), 5)
        self.assertEqual(
            {helper.stem for helper in helpers},
            {
                "felinaut Helper",
                "felinaut Helper (Renderer)",
                "felinaut Helper (GPU)",
                "felinaut Helper (Plugin)",
                "felinaut Helper (Alerts)",
            },
        )
        for helper in helpers:
            self.assertEqual(
                (helper / "Contents/MacOS" / helper.stem).read_bytes(),
                engines.HELPER.read_bytes(),
            )
            plist = plistlib.loads((helper / "Contents/Info.plist").read_bytes())
            self.assertTrue(plist["LSUIElement"])
            self.assertEqual(plist["CFBundleExecutable"], helper.stem)
        for name in self.bundle.LICENSES:
            notice = app / "Contents/Resources/cef" / name
            self.assertEqual(notice.read_bytes(), (engines.SDK / name).read_bytes())
            self.assertEqual(stat.S_IMODE(notice.stat().st_mode), 0o444)

    def test_webkit_only_does_not_read_sdk_or_helper(self):
        shutil.rmtree(engines.SDK.parent.parent)
        engines.HELPER.unlink()
        with patch.object(
            self.bundle, "verify_prepared_sdk", side_effect=AssertionError("SDK access")
        ):
            app = self.assemble(False)
        self.assertFalse((app / "Contents/Frameworks").exists())
        self.assertTrue((app / "Contents/MacOS/felinaut").is_file())

    def test_existing_app_is_not_overwritten(self):
        app = self.assemble()
        before = self.tree(app)
        with self.assertRaisesRegex(engines.EngineError, "already exists"):
            self.assemble()
        self.assertEqual(before, self.tree(app))

    def test_wrong_output_and_symlink_ancestry_are_refused(self):
        for output in (self.base / "outside", engines.TARGET / "other-prefix"):
            with self.assertRaises(engines.EngineError):
                engines.assemble(output, self.runtime, self.bundle)
        link = engines.TARGET / "felinaut-engines-link"
        link.symlink_to(self.output, target_is_directory=True)
        with self.assertRaises(self.bundle.BundleError):
            engines.assemble(link, self.runtime, self.bundle)

    def test_sdk_archive_corruption_fails_before_output(self):
        archive = engines.SDK.parent.parent / self.pin.name
        data = archive.read_bytes()
        archive.write_bytes(bytes([data[0] ^ 1]) + data[1:])
        with self.assertRaises(ValueError):
            self.assemble()
        self.assertEqual(list(self.output.iterdir()), [])

    def test_sdk_tree_modification_fails_before_output(self):
        (engines.SDK / "LICENSE.txt").write_bytes(b"tampered publisher notice")
        with self.assertRaises(ValueError):
            self.assemble()
        self.assertEqual(list(self.output.iterdir()), [])

    def test_missing_sdk_directory_is_not_created_by_verification(self):
        shutil.rmtree(engines.SDK)
        with patch.object(
            self.runtime, "private_directory", side_effect=AssertionError("SDK write")
        ):
            with self.assertRaisesRegex(engines.EngineError, "Prepared SDK is missing"):
                engines.verify_sdk(self.bundle, self.runtime.Deadline(5))
        self.assertFalse(engines.SDK.exists())
        self.assertTrue((engines.SDK.parent.parent / self.pin.name).is_file())

    def test_missing_sdk_does_not_acquire_it(self):
        archive = engines.SDK.parent.parent / self.pin.name
        archive.unlink()
        with patch(
            "urllib.request.build_opener", side_effect=AssertionError("download")
        ):
            with self.assertRaises(OSError):
                self.assemble()
        self.assertFalse(archive.exists())

    def test_redirected_and_invalid_binary_refused(self):
        engines.MAIN.unlink()
        engines.MAIN.symlink_to(engines.HELPER)
        with self.assertRaises(self.bundle.BundleError):
            self.assemble()
        engines.MAIN.unlink()
        for data in (b"", b"not a binary" * 10, self.macho(6, b"dylib not executable")):
            engines.MAIN.write_bytes(data)
            engines.MAIN.chmod(0o755)
            with self.assertRaises(self.bundle.BundleError):
                self.assemble()

    def test_hardlinked_root_and_helper_are_refused(self):
        engines.HELPER.unlink()
        os.link(engines.MAIN, engines.HELPER)
        with self.assertRaisesRegex(engines.EngineError, "distinct"):
            self.assemble()
        self.assertEqual(list(self.output.iterdir()), [])

    def test_copy_failure_removes_only_incomplete_app(self):
        sentinel = self.output / "build.log"
        sentinel.write_text("retain this diagnostic")
        original = self.bundle.copy_verified_file

        def fail_after_copy(*args, **kwargs):
            original(*args, **kwargs)
            raise OSError("simulated copy failure")

        with patch.object(
            self.bundle, "copy_verified_file", side_effect=fail_after_copy
        ):
            with self.assertRaisesRegex(OSError, "simulated copy"):
                self.assemble()
        self.assertEqual(list(self.output.iterdir()), [sentinel])

    def test_low_disk_budget_fails_before_app_creation(self):
        with patch.object(
            engines.shutil, "disk_usage", return_value=SimpleNamespace(free=0)
        ):
            with self.assertRaisesRegex(engines.EngineError, "disk budget"):
                self.assemble()
        self.assertEqual(list(self.output.iterdir()), [])

    def build_or_manifest(self, command, *, env, timeout, log):
        if command[0] == "cargo":
            return None
        self.assertEqual(command[1:], ["--list-engines"])
        self.assertEqual(command[0].name, "felinaut")
        self.assertEqual(command[0].parent.parent.parent.name, "Felinaut Engines.app")
        self.assertNotEqual(command[0], engines.MAIN)
        self.assertEqual(timeout, 10)
        self.assertNotIn("HOME", env)
        self.assertNotIn("CFFIXED_USER_HOME", env)
        self.assertNotIn("FELINAUT_NATIVE_TEST", env)
        self.assertNotIn("CEF_PATH", env)
        self.assertNotIn("FELINAUT_AGENT", env)
        log.write(json.dumps(engine_manifest()).encode())
        log.flush()

    def test_build_only_always_builds_both_and_never_launches(self):
        with (
            patch.object(engines.platform, "system", return_value="Darwin"),
            patch.object(engines.platform, "machine", return_value="arm64"),
            patch.object(
                engines, "probe_tools", return_value=(self.runtime, self.bundle)
            ),
            patch.object(
                engines, "run_command", side_effect=self.build_or_manifest
            ) as run,
            patch.object(
                engines, "launch", side_effect=AssertionError("unexpected launch")
            ),
            redirect_stdout(io.StringIO()) as output,
        ):
            self.assertEqual(engines.main(["--build"]), 0)
        result = json.loads(output.getvalue())
        self.assertEqual(run.call_count, 3)
        self.assertTrue(Path(result["app"]).is_dir())
        self.assertEqual(
            json.loads((Path(result["output"]) / "engines.json").read_text()),
            engine_manifest(),
        )
        for call in run.call_args_list[:2]:
            self.assertEqual(call.kwargs["env"]["CEF_PATH"], str(engines.SDK))
            self.assertEqual(call.kwargs["timeout"], 600)

    def test_run_implies_fresh_build_and_retains_launched_app_without_waiting(self):
        process = Mock(pid=123)
        with (
            patch.object(engines.platform, "system", return_value="Darwin"),
            patch.object(engines.platform, "machine", return_value="arm64"),
            patch.object(
                engines, "probe_tools", return_value=(self.runtime, self.bundle)
            ),
            patch.object(
                engines, "run_command", side_effect=self.build_or_manifest
            ) as build,
            patch.object(engines.subprocess, "Popen", return_value=process),
            patch.object(
                engines,
                "stop_owned_root",
                side_effect=AssertionError("interactive process must stay running"),
            ),
            redirect_stdout(io.StringIO()) as output,
        ):
            self.assertEqual(engines.main(["--run"]), 0)
        result = json.loads(output.getvalue())
        self.assertEqual(build.call_count, 3)
        self.assertEqual(result["pid"], 123)
        self.assertTrue(Path(result["app"]).is_dir())
        self.assertTrue(Path(result["home"]).is_dir())
        self.assertFalse((Path(result["home"]) / engines.MARKER_NAME).exists())
        process.wait.assert_not_called()
        self.assertEqual(
            json.loads((Path(result["output"]) / "result.json").read_text()), result
        )

    def test_webkit_only_build_verifies_copied_app_without_cef_enabled(self):
        def command_result(command, *, env, timeout, log):
            if command[0] != "cargo":
                self.assertEqual(command[1:], ["--list-engines"])
                self.assertEqual(timeout, 10)
                log.write(json.dumps(engine_manifest(False)).encode())
                log.flush()

        with (
            patch.object(engines.platform, "system", return_value="Darwin"),
            patch.object(engines.platform, "machine", return_value="arm64"),
            patch.object(
                engines, "probe_tools", return_value=(self.runtime, self.bundle)
            ),
            patch.object(engines, "run_command", side_effect=command_result) as run,
            patch.object(engines, "launch", side_effect=AssertionError("launch")),
            redirect_stdout(io.StringIO()) as output,
        ):
            self.assertEqual(engines.main(["--build", "--webkit-only"]), 0)
        self.assertEqual(run.call_count, 2)
        result = json.loads(output.getvalue())
        self.assertEqual(
            json.loads((Path(result["output"]) / "engines.json").read_text()),
            engine_manifest(False),
        )

    def test_concurrent_cargo_overwrite_is_caught_in_assembled_app_for_all_modes(self):
        overwritten = self.macho(2, b"concurrent WebKit-only Cargo artifact")

        def command_result(command, *, env, timeout, log):
            if command[0] == "cargo":
                if "--manifest-path" in command:
                    engines.MAIN.write_bytes(overwritten)
                return None
            # The actual copy reflects the overwritten binary, not the build's
            # requested features. Querying target/debug instead would be wrong.
            self.assertEqual(command[1:], ["--list-engines"])
            self.assertNotEqual(command[0], engines.MAIN)
            self.assertEqual(command[0].read_bytes(), overwritten)
            log.write(json.dumps(engine_manifest(False)).encode())
            log.flush()

        for mode in ("--build", "--run", "--smoke"):
            with (
                self.subTest(mode=mode),
                patch.object(engines.platform, "system", return_value="Darwin"),
                patch.object(engines.platform, "machine", return_value="arm64"),
                patch.object(
                    engines, "probe_tools", return_value=(self.runtime, self.bundle)
                ),
                patch.object(engines, "run_command", side_effect=command_result),
                patch.object(
                    engines, "create_profile", side_effect=AssertionError("profile")
                ),
                patch.object(
                    engines,
                    "launch",
                    side_effect=AssertionError("wrong build launched"),
                ),
                patch.object(
                    engines, "smoke", side_effect=AssertionError("wrong build smoke")
                ),
                redirect_stdout(io.StringIO()) as output,
                patch("sys.stderr", io.StringIO()) as diagnostic,
            ):
                with self.assertRaises(SystemExit) as error:
                    engines.main([mode])
                self.assertEqual(error.exception.code, 1)
                self.assertEqual(output.getvalue(), "")
                self.assertIn(
                    "concurrent Cargo artifact overwrite", diagnostic.getvalue()
                )
                self.assertIn("sequentially", diagnostic.getvalue())
                self.assertIn("enabled set mismatch", diagnostic.getvalue())
        for directory in engines.TARGET.glob("felinaut-engines-*"):
            self.assertFalse((directory / "result.json").exists())
            self.assertFalse((directory / "engines.json").exists())

    def test_build_failure_never_assembles_or_launches(self):
        with (
            patch.object(engines.platform, "system", return_value="Darwin"),
            patch.object(engines.platform, "machine", return_value="arm64"),
            patch.object(
                engines, "probe_tools", return_value=(self.runtime, self.bundle)
            ),
            patch.object(
                engines, "run_command", side_effect=engines.EngineError("build timeout")
            ),
            patch.object(engines, "assemble", side_effect=AssertionError("stale app")),
            patch.object(engines, "launch", side_effect=AssertionError("launch")),
            patch("sys.stderr", io.StringIO()),
        ):
            with self.assertRaises(SystemExit) as error:
                engines.main(["--run"])
        self.assertEqual(error.exception.code, 1)


class ProfileTests(PrivateFixture):
    def setUp(self):
        super().setUp()
        self.patch_value(engines, "TEMP_ROOT", self.base)

    def test_interactive_home_fresh_private_without_native_test_or_mock_keychain(self):
        home = engines.create_profile()
        env = engines.launch_environment(
            home,
            {
                "HOME": "/real",
                "TMPDIR": "/real/tmp",
                "FELINAUT_NATIVE_TEST": "1",
                "FELIFOX_NATIVE_TEST": "1",
                "CEF_FLAGS": "--use-mock-keychain",
                "DYLD_LIBRARY_PATH": "/evil",
                "TOKEN": "secret",
                "LANG": "en_US.UTF-8",
            },
        )
        self.assertEqual(
            env,
            {
                "HOME": str(home),
                "CFFIXED_USER_HOME": str(home),
                "FELINAUT_AGENT": "0",
                "LANG": "en_US.UTF-8",
                "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
            },
        )
        self.assertEqual(stat.S_IMODE(home.stat().st_mode), 0o700)
        self.assertEqual(list(home.iterdir()), [])

    def test_smoke_home_has_exact_marker_and_native_test(self):
        home = engines.create_profile(smoke=True)
        self.assertTrue(home.name.startswith("felinaut-engines-test-"))
        self.assertEqual(
            (home / engines.MARKER_NAME).read_bytes(), b"felinaut-engine-test-v1\n"
        )
        env = engines.launch_environment(home, {}, smoke=True)
        self.assertEqual(env["FELINAUT_NATIVE_TEST"], "1")
        self.assertEqual(env["FELINAUT_AGENT"], "1")
        self.assertEqual(env["HOME"], env["CFFIXED_USER_HOME"])
        self.assertNotIn("CEF_FLAGS", env)
        with self.assertRaises(engines.EngineError):
            engines.launch_environment(home, {}, smoke=False)

    def test_reused_or_nonprivate_profile_is_refused(self):
        for smoke in (True, False):
            with self.subTest(smoke=smoke):
                home = engines.create_profile(smoke)
                (home / "old-session").write_text("not fresh")
                with self.assertRaisesRegex(engines.EngineError, "fresh"):
                    engines.launch_environment(home, {}, smoke=smoke)
                (home / "old-session").unlink()
                home.chmod(0o755)
                with self.assertRaisesRegex(engines.EngineError, "0700"):
                    engines.launch_environment(home, {}, smoke=smoke)

    def test_invalid_marker_and_symlink_home_are_refused(self):
        home = engines.create_profile(True)
        marker = home / engines.MARKER_NAME
        marker.write_bytes(b"wrong marker\n")
        with self.assertRaisesRegex(engines.EngineError, "marker"):
            engines.launch_environment(home, {}, smoke=True)
        marker.unlink()
        other = self.base / "other"
        other.write_bytes(engines.MARKER)
        marker.symlink_to(other)
        with self.assertRaisesRegex(engines.EngineError, "marker"):
            engines.launch_environment(home, {}, smoke=True)
        link = self.base / "felinaut-engines-test-link"
        link.symlink_to(home, target_is_directory=True)
        with self.assertRaises(engines.EngineError):
            engines.launch_environment(link, {}, smoke=True)

    def test_launch_detaches_only_own_app_and_returns_without_waiting(self):
        home = engines.create_profile()
        app = self.base / "app.app"
        process = Mock(pid=123)
        with patch.object(engines.subprocess, "Popen", return_value=process) as popen:
            actual, env = engines.launch(app, home, self.base)
        self.assertIs(actual, process)
        process.wait.assert_not_called()
        process.terminate.assert_not_called()
        self.assertEqual(
            popen.call_args.args[0], [str(app / "Contents/MacOS/felinaut")]
        )
        self.assertTrue(popen.call_args.kwargs["start_new_session"])
        self.assertEqual(popen.call_args.kwargs["stdin"], subprocess.DEVNULL)
        self.assertEqual(env["HOME"], str(home))
        self.assertTrue(home.is_dir())
        self.assertEqual(
            stat.S_IMODE((self.base / "browser.log").stat().st_mode), 0o600
        )


class ServerTests(unittest.TestCase):
    def test_real_loopback_fixture_and_reject_other_routes_hosts_methods(self):
        with engines.fixture_server() as url:
            port = int(url.split(":")[-1].rstrip("/"))
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
            connection.request("GET", "/")
            response = connection.getresponse()
            self.assertEqual(response.status, 200)
            page = response.read()
            document = re.search(rb'data-fixture-document="([0-9a-f]{32})"', page)
            self.assertIsNotNone(document)
            self.assertEqual(
                page,
                engines.FIXTURE.read_bytes().replace(
                    b"__FELINAUT_DOCUMENT_ID__", document[1]
                ),
            )
            self.assertEqual(int(response.getheader("Content-Length")), len(page))
            self.assertEqual(response.getheader("Cache-Control"), "no-store")
            policy = response.getheader("Content-Security-Policy")
            self.assertIn("connect-src 'none'", policy)
            self.assertIn("script-src 'unsafe-inline' 'unsafe-eval'", policy)
            self.assertIn("frame-src 'none'", policy)
            connection.close()
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
            connection.request("GET", "/")
            second = connection.getresponse().read()
            self.assertNotEqual(second, page)
            self.assertEqual(
                re.sub(rb"[0-9a-f]{32}", b"DOCUMENT", second),
                re.sub(rb"[0-9a-f]{32}", b"DOCUMENT", page),
            )
            connection.close()
            for path in (
                "/missing",
                "/../README.md",
                "/?url=https://example.test",
                "/favicon.ico",
            ):
                connection = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
                connection.request("GET", path)
                self.assertEqual(connection.getresponse().status, 404)
                connection.close()
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
            connection.request("GET", "/", headers={"Host": "example.test"})
            self.assertEqual(connection.getresponse().status, 404)
            connection.close()
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
            connection.request("POST", "/", body=b"no writes")
            self.assertEqual(connection.getresponse().status, 501)
            connection.close()
        with socket.socket() as client:
            client.settimeout(1)
            self.assertNotEqual(client.connect_ex(("127.0.0.1", port)), 0)


class ControlValidationTests(unittest.TestCase):
    @staticmethod
    def status():
        return {
            "selected": "webkit",
            "pending": None,
            "error": None,
            "available": [
                {"id": engine, "ready": True, "error": None}
                for engine in ("webkit", "cef")
            ],
        }

    def test_engine_status_supports_pending_but_never_silent_unavailability(self):
        status = self.status()
        self.assertIs(engines.engine_status(status), status)
        status["pending"] = "cef"
        self.assertIs(engines.engine_status(status), status)
        for field, value in (
            ("selected", "gecko"),
            ("pending", "unknown"),
            ("error", "failed"),
            ("available", []),
            ("available", [None]),
        ):
            with self.subTest(field=field, value=value):
                invalid = self.status()
                invalid[field] = value
                with self.assertRaises(engines.EngineError):
                    engines.engine_status(invalid)
        for field in ("pending", "error", "available", "selected"):
            invalid = self.status()
            del invalid[field]
            with self.assertRaises(engines.EngineError):
                engines.engine_status(invalid)
        for failure in (
            {"ready": False},
            {"error": "private runtime failure"},
            {"id": "webkit"},
        ):
            invalid = self.status()
            invalid["available"][1].update(failure)
            with self.assertRaises(engines.EngineError):
                engines.engine_status(invalid)

    def test_active_tab_checks_count_selection_and_errors(self):
        tab = {"active": True, "engine": "cef", "error": None, "switch_error": None}
        self.assertIs(engines.active_tab([tab], 1), tab)
        for tabs, count in (
            (None, 1),
            ([], 1),
            ([None], 1),
            ([tab, tab], 2),
            ([tab], 2),
            ([{"active": False}], 1),
        ):
            with self.assertRaises(engines.EngineError):
                engines.active_tab(tabs, count)
        for field in ("error", "switch_error"):
            with self.assertRaises(engines.EngineError):
                engines.active_tab([{**tab, field: "private failure"}], 1)

    def test_snapshot_references_require_exact_single_control(self):
        snapshot = '@e1 textbox "Test input" = ""\n@e2 button "Verify input"\n'
        self.assertEqual(engines.snapshot_ref(snapshot, "textbox", "Test input"), "@e1")
        for invalid in ("", '@e1 textbox "Test input extra"', snapshot + snapshot):
            with self.assertRaises(engines.EngineError):
                engines.snapshot_ref(invalid, "textbox", "Test input")

    def test_expired_time_budget_fails_before_more_commands(self):
        with self.assertRaisesRegex(engines.EngineError, "budget exhausted"):
            engines.remaining_seconds(0)

    def test_command_error_keeps_expected_diagnostic_private_and_bounded(self):
        with patch.object(
            engines.subprocess,
            "run",
            side_effect=subprocess.CalledProcessError(
                1, "ctl", stderr="confirmation required; private details"
            ),
        ):
            with self.assertRaises(engines.CommandError) as caught:
                engines.run_command(["ctl"], env={}, timeout=1)
        self.assertEqual(caught.exception.returncode, 1)
        self.assertIn("confirmation", caught.exception.diagnostic)
        self.assertNotIn("private", str(caught.exception))
        self.assertEqual(
            len(engines.CommandError("ctl", 1, "x" * 10000).diagnostic), 8192
        )


class RawControlTests(unittest.TestCase):
    def setUp(self):
        # Keep the Unix socket below Darwin's path limit. No actual browser runs.
        temporary = tempfile.TemporaryDirectory(
            prefix="felinaut-engines-test-", dir=Path("/tmp").resolve()
        )
        self.addCleanup(temporary.cleanup)
        self.home = Path(temporary.name)
        self.home.chmod(0o700)
        change = patch.object(engines, "TEMP_ROOT", self.home.parent)
        change.start()
        self.addCleanup(change.stop)
        with engines.private_file(self.home / engines.MARKER_NAME) as marker:
            marker.write(engines.MARKER)
        self.directory = self.home / "Library/Application Support/Felifox"
        self.directory.mkdir(parents=True)
        self.token = self.directory / "agent.token"
        with engines.private_file(self.token) as token:
            token.write(b"offline-token-not-an-account-secret")
        self.socket = self.directory / "agent.sock"
        self.requests = []

    @contextmanager
    def server(self, response):
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        listener.bind(str(self.socket))
        self.socket.chmod(0o600)
        listener.listen(1)
        listener.settimeout(2)
        errors = []

        def serve():
            try:
                connection, _ = listener.accept()
                with connection:
                    connection.settimeout(2)
                    data = b""
                    while b"\n" not in data:
                        chunk = connection.recv(4096)
                        if not chunk:
                            raise RuntimeError("Incomplete test request")
                        data += chunk
                    self.requests.append(json.loads(data))
                    connection.sendall(response)
            except (BrokenPipeError, ConnectionResetError):
                pass  # Oversized-response rejection can close the client early.
            except BaseException as error:
                errors.append(error)

        thread = threading.Thread(target=serve, daemon=True)
        thread.start()
        try:
            yield
        finally:
            thread.join(timeout=3)
            listener.close()
            self.socket.unlink(missing_ok=True)
            self.assertFalse(thread.is_alive())
            self.assertEqual(errors, [])

    def test_real_authenticated_request_reaches_only_private_marked_socket(self):
        with self.server(b'{"ok":false,"error":"Unknown engine id"}\n'):
            engines.invalid_engine_request(self.home, timeout=2)
        self.assertEqual(
            self.requests,
            [
                {
                    "cmd": "engine",
                    "engine": "invalid-engine",
                    "confirm": True,
                    "token": self.token.read_text(),
                }
            ],
        )

    def test_success_auth_failure_malformed_truncated_and_oversized_are_not_rejections(
        self,
    ):
        for response in (
            b'{"ok":true,"value":null}\n',
            b'{"ok":false,"error":"Wrong token"}\n',
            b'{"ok":false,"error":""}\n',
            b'{"ok":false,"error":42}\n',
            b"[]\n",
            b"not json\n",
            b'{"ok":false}',
            b"x" * 65537,
        ):
            with self.subTest(response_kind=len(response)), self.server(response):
                with self.assertRaises(engines.EngineError):
                    engines.invalid_engine_request(self.home, timeout=2)

    def test_symlink_or_nonprivate_token_refused_before_connecting(self):
        self.token.chmod(0o644)
        with self.assertRaisesRegex(engines.EngineError, "private"):
            engines.invalid_engine_request(self.home, timeout=1)
        self.token.unlink()
        other = self.home / "other-token"
        other.write_text("not accepted")
        self.token.symlink_to(other)
        with self.assertRaisesRegex(engines.EngineError, "redirected"):
            engines.invalid_engine_request(self.home, timeout=1)

    def test_unmarked_home_cannot_send_raw_commands(self):
        (self.home / engines.MARKER_NAME).write_bytes(b"wrong marker\n")
        with self.assertRaisesRegex(engines.EngineError, "marker"):
            engines.invalid_engine_request(self.home, timeout=1)


class FakeMixedBrowser:
    """Behavioral offline model; no executable or actual engine is launched."""

    def __init__(self, test):
        self.test = test
        self.documents = 0
        self.tabs = [self.page("webkit")]
        self.index = 0
        self.contexts = {
            engine: {"cookie": None, "storage": None} for engine in ("webkit", "cef")
        }
        self.references = {}
        self.snapshot_number = 0
        self.commands = []
        self.quit_requested = False
        self.exit_code = 0
        self.shared_cross_engine_context = False
        self.separate_cef_tabs = False
        self.preserve_document_on_switch = False
        self.wrong_tab_closed = False
        self.switch_adds_tab = False
        self.accepts_without_confirmation = False
        self.wrong_rejection = False
        self.rejection_clears_input = False
        self.not_ready_once = False
        self.wrong_backend = False
        self.engine_failure = False
        self.tab_failure = False
        self.pending_polls = 0
        self.new_tab_index = 0
        self.malformed_state = False

    def page(self, engine, url="felinaut://newtab"):
        self.documents += 1
        return {
            "engine": engine,
            "url": url,
            "document": f"{self.documents:032x}",
            "input": "",
            "result": None,
            "trusted": None,
        }

    @property
    def current(self):
        return self.tabs[self.index]

    def context(self):
        engine = (
            "webkit" if self.shared_cross_engine_context else self.current["engine"]
        )
        return self.contexts[engine]

    def run(self, command, *, env, timeout):
        self.test.assertEqual(command[0], engines.CLIENT)
        self.test.assertEqual(Path(env["HOME"]).parent, self.test.base)
        self.test.assertEqual(env["CFFIXED_USER_HOME"], env["HOME"])
        self.test.assertEqual(env["FELINAUT_NATIVE_TEST"], "1")
        self.test.assertEqual(env["FELINAUT_AGENT"], "1")
        self.test.assertGreater(timeout, 0)
        self.test.assertLessEqual(timeout, 3)
        name, *args = command[1:]
        self.commands.append((name, *args))
        if name == "tabs":
            value = [
                {
                    "index": index,
                    "url": tab["url"],
                    "engine": tab["engine"],
                    "active": index == self.index,
                    "loading": False,
                    "error": "private error must not be logged"
                    if self.tab_failure
                    else None,
                    "switch_error": None,
                }
                for index, tab in enumerate(self.tabs)
            ]
        elif name == "engine" and not args:
            value = {
                "selected": self.current["engine"],
                "pending": "cef" if self.pending_polls else None,
                "error": None,
                "available": [
                    {"id": engine, "ready": not self.engine_failure, "error": None}
                    for engine in ("webkit", "cef")
                ],
            }
            self.pending_polls = max(0, self.pending_polls - 1)
        elif name == "backend":
            cef = self.current["engine"] == "cef"
            value = {
                "engine_family": "wrong-engine"
                if self.wrong_backend
                else "Chromium"
                if cef
                else "WebKit",
                "backend": "cef" if cef else "native-webkit",
                "runtime_version": "offline-fixture-version",
            }
        elif name == "navigate":
            self.test.assertRegex(args[0], r"^http://127\.0\.0\.1:[0-9]+/$")
            self.tabs[self.index] = self.page(self.current["engine"], args[0])
            value = None
        elif name == "new-tab":
            if self.separate_cef_tabs and args[0] == "cef":
                self.contexts["cef"] = {"cookie": None, "storage": None}
            self.tabs.insert(0, self.page(args[0]))
            self.index = 0
            value = self.new_tab_index
        elif name == "tab":
            self.index = int(args[0])
            value = None
        elif name == "close-tab":
            self.tabs.pop(1 if self.wrong_tab_closed else self.index)
            self.index = 0
            value = None
        elif name == "engine":
            if len(args) == 1 and not self.accepts_without_confirmation:
                if self.rejection_clears_input:
                    self.current["input"] = ""
                raise engines.CommandError(
                    str(engines.CLIENT),
                    1,
                    "does not listen"
                    if self.wrong_rejection
                    else "Reload confirmation required",
                )
            if self.switch_adds_tab:
                self.tabs.insert(0, self.page(args[0], self.current["url"]))
                self.index = 0
            elif self.preserve_document_on_switch:
                self.current["engine"] = args[0]
            else:
                self.tabs[self.index] = self.page(args[0], self.current["url"])
            value = None
        elif name == "snapshot":
            self.snapshot_number += 2
            input_ref, button_ref = (
                f"@e{self.snapshot_number}",
                f"@e{self.snapshot_number + 1}",
            )
            self.references = {input_ref: "input", button_ref: "button"}
            return f'{input_ref} textbox "Test input" = {json.dumps(self.current["input"])}\n{button_ref} button "Verify input"\n'
        elif name == "type":
            self.test.assertEqual(self.references.get(args[0]), "input")
            self.current["input"] = args[1]
            self.references.clear()
            value = None
        elif name == "click":
            self.test.assertEqual(self.references.get(args[0]), "button")
            marker = self.current["input"]
            self.context().update(cookie=marker, storage=marker)
            self.current.update(result=marker, trusted="false")
            self.references.clear()
            value = None
        elif name == "eval":
            self.test.assertEqual(args, [engines.STATE_SCRIPT])
            if self.not_ready_once:
                self.not_ready_once = False
                return json.dumps({"ready": False})
            value = (
                True
                if self.malformed_state
                else {
                    "ready": True,
                    "document": self.current["document"],
                    **self.context(),
                    **{
                        key: self.current[key] for key in ("input", "result", "trusted")
                    },
                }
            )
        elif name == "quit":
            self.quit_requested = True
            value = None
        else:
            self.test.fail(f"Unexpected CLI command {name}")
        return "" if value is None else json.dumps(value)

    def reject_invalid(self, home, *, timeout):
        engines.validate_profile(home, smoke=True)
        self.test.assertEqual(home.parent, self.test.base)
        self.test.assertGreater(timeout, 0)
        self.commands.append(("invalid-engine-request",))

    def wait(self, *, timeout):
        self.test.assertTrue(self.quit_requested)
        self.test.assertGreater(timeout, 0)
        self.test.assertLessEqual(timeout, 15)
        self.test.process.returncode = self.exit_code
        return self.exit_code


class SmokeTests(PrivateFixture):
    def setUp(self):
        super().setUp()
        self.patch_value(engines, "TEMP_ROOT", self.base)
        self.output = self.base / "output"
        self.output.mkdir()
        self.process = Mock(pid=123, returncode=None)
        self.process.poll.side_effect = lambda: self.process.returncode
        self.browser = FakeMixedBrowser(self)
        self.process.wait.side_effect = self.browser.wait

    @contextmanager
    def native_mocks(self):
        with (
            patch.object(engines.subprocess, "Popen", return_value=self.process),
            patch.object(engines, "run_command", side_effect=self.browser.run),
            patch.object(
                engines,
                "invalid_engine_request",
                side_effect=self.browser.reject_invalid,
            ),
            patch.object(engines, "stop_owned_root") as stop,
        ):
            yield stop

    def test_complete_mixed_smoke_quits_gracefully_and_keeps_private_evidence(self):
        self.browser.pending_polls = 1
        self.browser.not_ready_once = True
        with self.native_mocks() as stop:
            report = engines.smoke(self.base / "app.app", self.output)
        stop.assert_not_called()
        self.process.terminate.assert_not_called()
        self.process.kill.assert_not_called()
        self.process.wait.assert_called_once()
        self.assertEqual(report["status"], "passed")
        self.assertEqual(report["mixed_engine_validation"], "passed")
        self.assertTrue(report["graceful_quit"])
        self.assertEqual(report["root_returncode"], 0)
        self.assertEqual(len(report["checks"]), 12)
        self.assertIn("not independently verified", report["helper_exit"])
        self.assertIn("not independently instrumented", report["cef_shutdown"])
        self.assertEqual(set(report["backends"]), {"webkit", "cef"})
        self.assertEqual(
            [tab["engine"] for tab in self.browser.tabs], ["cef", "webkit"]
        )
        self.assertTrue(Path(report["home"]).is_dir())
        self.assertIn(("engine", "webkit"), self.browser.commands)
        self.assertIn(("engine", "webkit", "--confirm"), self.browser.commands)
        self.assertIn(("engine", "cef", "--confirm"), self.browser.commands)
        self.assertIn(("new-tab", "webkit"), self.browser.commands)
        self.assertEqual(self.browser.commands.count(("new-tab", "cef")), 2)
        self.assertEqual(self.browser.commands[-1], ("quit",))
        self.assertEqual(json.loads((self.output / "smoke.json").read_text()), report)

    def test_behavioral_regressions_fail_at_the_relevant_stage_and_cleanup_owned_root(
        self,
    ):
        cases = (
            ("shared_cross_engine_context", "cef-isolated-context-and-dom-input"),
            ("separate_cef_tabs", "second-cef-tab-shares-context"),
            ("wrong_tab_closed", "close-cef-tab-preserves-first-page"),
            (
                "accepts_without_confirmation",
                "unconfirmed-switch-rejected-without-page-change",
            ),
            ("wrong_rejection", "unconfirmed-switch-rejected-without-page-change"),
            (
                "rejection_clears_input",
                "unconfirmed-switch-rejected-without-page-change",
            ),
            ("preserve_document_on_switch", "confirmed-cef-to-webkit-switch"),
            ("switch_adds_tab", "confirmed-cef-to-webkit-switch"),
            ("wrong_backend", "startup"),
            ("engine_failure", "startup"),
            ("tab_failure", "startup"),
            ("malformed_state", "webkit-empty-context-and-dom-input"),
        )
        for number, (flag, stage) in enumerate(cases):
            with self.subTest(regression=flag):
                self.browser = FakeMixedBrowser(self)
                setattr(self.browser, flag, True)
                output = self.base / f"failure-{number}"
                output.mkdir()
                with self.native_mocks() as stop:
                    with self.assertRaises(engines.EngineError):
                        engines.smoke(self.base / "app.app", output)
                stop.assert_called_once_with(self.process)
                report = json.loads((output / "smoke.json").read_text())
                self.assertEqual(report["status"], "failed")
                self.assertEqual(report["stage"], stage)
                self.assertFalse(report["graceful_quit"])
                self.assertNotIn("private error", json.dumps(report))
                self.assertTrue(Path(report["home"]).is_dir())

    def test_new_tab_must_return_actual_zero_insertion_index(self):
        for number, index in enumerate((False, None, 1, "0")):
            with self.subTest(index=index):
                self.browser = FakeMixedBrowser(self)
                self.browser.new_tab_index = index
                output = self.base / f"index-{number}"
                output.mkdir()
                with self.native_mocks():
                    with self.assertRaisesRegex(
                        engines.EngineError, "insertion index|Malformed JSON"
                    ):
                        engines.smoke(self.base / "app.app", output)

    def test_failed_graceful_exit_is_never_reported_as_success(self):
        self.browser.exit_code = 1
        with self.native_mocks() as stop:
            with self.assertRaisesRegex(engines.EngineError, "exited unsuccessfully"):
                engines.smoke(self.base / "app.app", self.output)
        stop.assert_not_called()
        report = json.loads((self.output / "smoke.json").read_text())
        self.assertEqual(report["status"], "failed")
        self.assertEqual(report["root_returncode"], 1)
        self.assertFalse(report["graceful_quit"])

    def test_quit_timeout_fails_and_only_then_allows_owned_root_cleanup(self):
        self.process.wait.side_effect = subprocess.TimeoutExpired("own-root", 15)
        with self.native_mocks() as stop:
            with self.assertRaisesRegex(engines.EngineError, "quit timed out"):
                engines.smoke(self.base / "app.app", self.output)
        stop.assert_called_once_with(self.process)
        self.assertTrue(self.browser.quit_requested)
        self.assertEqual(
            json.loads((self.output / "smoke.json").read_text())["status"], "failed"
        )

    def test_capture_happens_with_cef_active_before_graceful_quit(self):
        def capture(process, output, end):
            self.assertEqual(self.browser.current["engine"], "cef")
            self.assertEqual(len(self.browser.tabs), 2)
            self.assertFalse(self.browser.quit_requested)
            self.assertIs(process, self.process)

        with (
            self.native_mocks() as stop,
            patch.object(engines, "capture_window", side_effect=capture) as window,
        ):
            report = engines.smoke(self.base / "app.app", self.output, capture=True)
        stop.assert_not_called()
        window.assert_called_once()
        self.assertEqual(report["window_capture"], "window.png")
        self.assertIn("owned-cef-browser-window-capture", report["checks"])

    def test_wait_timeout_and_exited_browser(self):
        with self.assertRaisesRegex(engines.EngineError, "timed out"):
            engines.wait_for(self.process, lambda seconds: False, 0)
        self.process.returncode = 1
        with self.assertRaisesRegex(engines.EngineError, "exited"):
            engines.wait_for(self.process, lambda seconds: True, float("inf"))

    def test_root_cleanup_never_kills_a_group_and_has_bounded_escalation(self):
        self.process.wait.side_effect = [subprocess.TimeoutExpired("own-root", 8), 0]
        engines.stop_owned_root(self.process)
        self.process.terminate.assert_called_once_with()
        self.process.kill.assert_called_once_with()
        self.assertEqual(
            [call.kwargs for call in self.process.wait.call_args_list],
            [
                {"timeout": 8},
                {"timeout": 5},
            ],
        )
        self.process.returncode = 0
        self.process.reset_mock()
        engines.stop_owned_root(self.process)
        self.process.terminate.assert_not_called()

    def test_capture_uses_only_owned_pid_window_and_never_display_fallback(self):
        import time

        with patch.object(
            engines, "run_command", side_effect=[None, "456\n", None]
        ) as run:
            engines.capture_window(self.process, self.output, time.monotonic() + 10)
        calls = run.call_args_list
        self.assertEqual(calls[1].args[0][-1], self.process.pid)
        capture = calls[2].args[0]
        self.assertEqual(
            capture[:5], ["/usr/sbin/screencapture", "-x", "-o", "-l", "456"]
        )
        with patch.object(
            engines, "run_command", side_effect=[None, "not a window"]
        ) as run:
            with self.assertRaisesRegex(engines.EngineError, "window ID"):
                engines.capture_window(self.process, self.output, time.monotonic() + 10)
        self.assertEqual(run.call_count, 2)


if __name__ == "__main__":
    unittest.main()
