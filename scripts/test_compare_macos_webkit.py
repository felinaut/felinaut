"""Offline comparison policy/orchestration tests: never launch a GUI or Swift."""

import contextlib
import copy
import importlib.util
import io
import json
import os
import plistlib
import shutil
import signal
import stat
import struct
import subprocess
import tempfile
import unittest
import zlib
from pathlib import Path
from unittest.mock import Mock, patch

import compare_macos_webkit as compare

UUID = "FBC915B7-CB20-3E09-BBE5-BC236EC075B9"
SYSTEM_UUID = "AC293B1A-9EAF-34D8-86B2-914D2ABB84F3"
BACKEND = {
    "backend": "native-webkit",
    "engine_family": "webkit",
    "runtime_version": "627.1.8+",
    "runtime_identity": {"source": "external-framework", "image_uuid": UUID},
    "capabilities": {name: True for name in compare.smoke.CAPABILITIES},
}


def engine_manifest(system=True, upstream=True):
    return {
        "schema_version": 1,
        "engines": [
            {
                "id": "system-webkit",
                "feature": "engine-webkit-system",
                "family": "webkit",
                "built": system,
                "adapter_implemented": True,
                "requires_isolated_launcher": False,
                "limitation": "System runtime; no external products implied",
            },
            {
                "id": "upstream-webkit",
                "feature": "engine-webkit-upstream",
                "family": "webkit",
                "built": upstream,
                "adapter_implemented": True,
                "requires_isolated_launcher": True,
                "limitation": "Requires separately prepared Cocoa WebKit products",
            },
        ],
    }


class ManifestTests(unittest.TestCase):
    def test_single_both_and_empty_build_selections(self):
        for system, upstream, expected in (
            (True, True, ("system", "upstream", "both")),
            (True, False, ("system",)),
            (False, True, ("upstream",)),
            (False, False, ()),
        ):
            with self.subTest(system=system, upstream=upstream):
                supported = compare.parse_engine_manifest(
                    json.dumps(engine_manifest(system, upstream))
                )
                self.assertEqual(supported, expected)
                for selected in ("system", "upstream", "both"):
                    if selected in expected:
                        compare.require_mode(selected, supported)
                    else:
                        with self.assertRaisesRegex(
                            compare.ComparisonError, "excluded.*rebuild:"
                        ):
                            compare.require_mode(selected, supported)

    def test_unimplemented_and_unknown_engines_are_never_offered(self):
        manifest = engine_manifest()
        manifest["engines"][1]["adapter_implemented"] = False
        for candidate in (
            "cef",
            "gecko",
            "servo",
            "ladybird",
            "ultralight",
            "wpe",
            "future",
        ):
            manifest["engines"].append(
                {
                    "id": candidate,
                    "feature": f"engine-{candidate}",
                    "family": candidate,
                    "built": False,
                    "adapter_implemented": False,
                    "requires_isolated_launcher": False,
                    "limitation": "Adapter not integrated",
                }
            )
        supported = compare.parse_engine_manifest(json.dumps(manifest))
        self.assertEqual(supported, ("system",))
        for selected in (
            "cef",
            "gecko",
            "servo",
            "ladybird",
            "ultralight",
            "wpe",
            "future",
        ):
            with (
                self.subTest(selected=selected),
                self.assertRaises(compare.ComparisonError),
            ):
                compare.require_mode(selected, supported)
        # An unknown future entry cannot enable a native comparison either.
        manifest["engines"][-1]["built"] = True
        manifest["engines"][-1]["adapter_implemented"] = True
        self.assertEqual(
            compare.parse_engine_manifest(json.dumps(manifest)), ("system",)
        )

    def test_old_missing_and_invalid_schema_has_actionable_rebuild_error(self):
        for text in ("", "not JSON", "null", "[]", "{}", '{"engines": []}'):
            with (
                self.subTest(text=text),
                self.assertRaisesRegex(
                    compare.ComparisonError,
                    "manifest:.*Rebuild: cargo build --locked --bins",
                ),
            ):
                compare.parse_engine_manifest(text)
        for version in (None, True, False, "1", 1.0, 0, 2):
            manifest = engine_manifest()
            manifest["schema_version"] = version
            with (
                self.subTest(version=version),
                self.assertRaises(compare.ComparisonError),
            ):
                compare.parse_engine_manifest(json.dumps(manifest))
        for entries in (None, {}, [], [None], [engine_manifest()["engines"][0]]):
            manifest = engine_manifest()
            manifest["engines"] = entries
            with (
                self.subTest(entries=entries),
                self.assertRaises(compare.ComparisonError),
            ):
                compare.parse_engine_manifest(json.dumps(manifest))

    def test_every_boolean_field_rejects_coercion_and_missing_values(self):
        for field in ("built", "adapter_implemented", "requires_isolated_launcher"):
            for invalid in (0, 1, "false", "true", None, [], {}):
                manifest = engine_manifest()
                manifest["engines"][1][field] = invalid
                with (
                    self.subTest(field=field, invalid=invalid),
                    self.assertRaisesRegex(
                        compare.ComparisonError, "must be a boolean"
                    ),
                ):
                    compare.parse_engine_manifest(json.dumps(manifest))
            manifest = engine_manifest()
            del manifest["engines"][0][field]
            with self.assertRaises(compare.ComparisonError):
                compare.parse_engine_manifest(json.dumps(manifest))

    def test_malformed_entry_strings_or_native_identity_are_rejected(self):
        for field in ("id", "feature", "family", "limitation"):
            for invalid in (None, 1, [], {}):
                manifest = engine_manifest()
                manifest["engines"][0][field] = invalid
                with (
                    self.subTest(field=field, invalid=invalid),
                    self.assertRaises(compare.ComparisonError),
                ):
                    compare.parse_engine_manifest(json.dumps(manifest))
            manifest = engine_manifest()
            del manifest["engines"][0][field]
            with self.assertRaises(compare.ComparisonError):
                compare.parse_engine_manifest(json.dumps(manifest))
        for field, value in (
            ("id", "other"),
            ("feature", "engine-other"),
            ("family", "wpe"),
        ):
            manifest = engine_manifest()
            manifest["engines"][0][field] = value
            with self.assertRaises(compare.ComparisonError):
                compare.parse_engine_manifest(json.dumps(manifest))

    def test_duplicate_ids_features_and_json_object_keys_are_rejected(self):
        manifest = engine_manifest()
        manifest["engines"].append(copy.deepcopy(manifest["engines"][0]))
        with self.assertRaisesRegex(compare.ComparisonError, "Duplicate engine"):
            compare.parse_engine_manifest(json.dumps(manifest))
        manifest["engines"][-1]["id"] = "other"
        with self.assertRaisesRegex(compare.ComparisonError, "Duplicate engine"):
            compare.parse_engine_manifest(json.dumps(manifest))
        for text in (
            json.dumps(engine_manifest()).replace(
                '"schema_version": 1', '"schema_version": 0, "schema_version": 1'
            ),
            json.dumps(engine_manifest()).replace(
                '"built": true', '"built": false, "built": true', 1
            ),
        ):
            with self.assertRaisesRegex(
                compare.ComparisonError, "Duplicate manifest field"
            ):
                compare.parse_engine_manifest(text)

    def test_preflight_uses_bounded_sanitized_command_without_creating_home(self):
        inherited = {
            "HOME": "/user/home",
            "CFFIXED_USER_HOME": "/user/home",
            "PATH": "/usr/bin:/bin",
            "FELINAUT_AGENT": "1",
            "DYLD_FRAMEWORK_PATH": "/old/products",
            "__XPC_DYLD_LIBRARY_PATH": "/old",
            "WEBKIT_UNSET_DYLD_FRAMEWORK_PATH": "NO",
            "FELINAUT_COMPARISON": "upstream",
            "FELINAUT_EXPECTED_WEBKIT_UUID": UUID,
        }
        with (
            tempfile.TemporaryDirectory() as name,
            patch.object(compare, "ROOT", Path(name)),
            patch.object(
                compare.smoke, "run", return_value=json.dumps(engine_manifest())
            ) as run,
            patch.object(compare, "private_home") as home,
        ):
            browser = executable(Path(name) / "target/debug/felinaut")
            self.assertEqual(
                compare.built_modes(inherited), ("system", "upstream", "both")
            )
        home.assert_not_called()
        self.assertEqual(run.call_args.args, (browser, "--list-engines"))
        self.assertEqual(run.call_args.kwargs["timeout"], 12)
        environment = run.call_args.kwargs["env"]
        self.assertEqual(
            environment,
            {
                "HOME": "/dev/null",
                "CFFIXED_USER_HOME": "/dev/null",
                "PATH": "/usr/bin:/bin",
                "FELINAUT_AGENT": "0",
            },
        )
        self.assertEqual(inherited["HOME"], "/user/home")

    def test_missing_binary_and_preflight_command_failure_request_rebuild(self):
        with (
            tempfile.TemporaryDirectory() as name,
            patch.object(compare, "ROOT", Path(name)),
        ):
            with patch.object(compare.smoke, "run") as run:
                with self.assertRaisesRegex(
                    compare.ComparisonError, "Missing executable.*rebuild:"
                ):
                    compare.built_modes({})
                run.assert_not_called()
            executable(Path(name) / "target/debug/felinaut")
            for failure in ("unknown --list-engines option", "timed out after 12s"):
                with (
                    patch.object(
                        compare.smoke,
                        "run",
                        side_effect=compare.smoke.SmokeError(failure),
                    ),
                    self.assertRaisesRegex(
                        compare.ComparisonError, "--list-engines.*rebuild:"
                    ),
                ):
                    compare.built_modes({})


def chunk(kind, payload=b""):
    return (
        struct.pack(">I", len(payload))
        + kind
        + payload
        + struct.pack(">I", zlib.crc32(kind + payload))
    )


PNG = (
    compare.smoke.PNG_SIGNATURE
    + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
    + chunk(b"IDAT", zlib.compress(b"\0\x10\x20\x30"))
    + chunk(b"IEND")
)


def executable(path):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"fake executable; never run")
    path.chmod(0o700)
    return path


class PolicyTests(unittest.TestCase):
    def test_parser_accepts_only_explicit_engine_names(self):
        for engine in ("system", "upstream", "both"):
            self.assertEqual(
                compare.parser().parse_args(["--engine", engine]).engine, engine
            )
        for engine in ("wpe", "System", "", "system "):
            with (
                contextlib.redirect_stderr(io.StringIO()),
                self.assertRaises(SystemExit),
            ):
                compare.parser().parse_args(["--engine", engine])
        for option in ("--url", "--skip-validation", "--disable-security"):
            with (
                contextlib.redirect_stderr(io.StringIO()),
                self.assertRaises(SystemExit),
            ):
                compare.parser().parse_args([option, "https://example.com"])

    def test_uuid_extraction_selects_host_not_first_slice(self):
        data = f"UUID: {SYSTEM_UUID} (x86_64) a path with spaces\nUUID: {UUID.lower()} (arm64) a path with spaces"
        self.assertEqual(compare.extract_uuid(data, "arm64"), UUID)
        self.assertEqual(compare.extract_uuid(data, "x86_64"), SYSTEM_UUID)

    def test_uuid_extraction_rejects_missing_malformed_and_duplicate_host_slices(self):
        good = f"UUID: {UUID} (arm64) /products/WebKit"
        for data in (
            "",
            "dwarfdump failed",
            good.replace("arm64", "arm64e"),
            good.replace(UUID, "garbage"),
            good + "\n" + good,
        ):
            with self.subTest(data=data), self.assertRaises(compare.ComparisonError):
                compare.extract_uuid(data, "arm64")
        for value in (None, 1, "", UUID.replace("-", ""), " " + UUID, UUID + "\n"):
            with self.subTest(value=value), self.assertRaises(compare.ComparisonError):
                compare.canonical_uuid(value)

    def test_identity_requires_actual_source_and_matching_uuid_not_version(self):
        self.assertEqual(compare.validate_identity(BACKEND, "upstream", UUID), BACKEND)
        system = copy.deepcopy(BACKEND)
        system["runtime_identity"] = {
            "source": "system-shared-cache",
            "image_uuid": SYSTEM_UUID,
        }
        # Identical version strings are fine: they do not decide runtime identity.
        self.assertEqual(compare.validate_identity(system, "system"), system)
        with self.assertRaisesRegex(compare.ComparisonError, "no fallback"):
            compare.validate_identity(system, "upstream", SYSTEM_UUID)
        with self.assertRaisesRegex(compare.ComparisonError, "no fallback"):
            compare.validate_identity(BACKEND, "system")
        with self.assertRaisesRegex(compare.ComparisonError, "UUID does not match"):
            compare.validate_identity(BACKEND, "upstream", SYSTEM_UUID)

    def test_unknown_or_incomplete_runtime_fails_closed(self):
        for identity in (
            None,
            {},
            {"source": "unverified", "image_uuid": UUID},
            {"source": "external-framework", "image_uuid": None},
            {"source": "external-framework", "image_uuid": UUID.lower()},
        ):
            info = dict(BACKEND, runtime_identity=identity)
            with (
                self.subTest(identity=identity),
                self.assertRaises(compare.ComparisonError),
            ):
                compare.validate_identity(info, "upstream", UUID)
        with self.assertRaises(compare.ComparisonError):
            compare.validate_identity(BACKEND, "upstream")
        with self.assertRaises(compare.ComparisonError):
            compare.validate_identity(BACKEND, "wpe", UUID)
        with self.assertRaises(compare.smoke.SmokeError):
            compare.validate_identity(dict(BACKEND, backend="wpe"), "upstream", UUID)

    def test_environment_scrubs_every_inherited_loader_and_comparison_override(self):
        inherited = {
            "PATH": "/usr/bin:/bin",
            "HOME": "/real/home",
            "KEEP_ME": "value",
            "DYLD_INSERT_LIBRARIES": "evil",
            "DYLD_FRAMEWORK_PATH": "/old",
            "__XPC_DYLD_LIBRARY_PATH": "/old",
            "__XPC_DYLD_INSERT_LIBRARIES": "evil",
            "WEBKIT_UNSET_DYLD_FRAMEWORK_PATH": "NO",
            "WEBKIT_OTHER": "old",
            "FELINAUT_COMPARISON": "old",
            "FELINAUT_COMPARISON_HOME": "/real/home",
            "FELINAUT_COMPARISON_FUTURE": "old",
            "FELINAUT_EXPECTED_WEBKIT_UUID": SYSTEM_UUID,
        }
        original = dict(inherited)
        with compare.private_home() as home:
            system = compare.browser_environment(inherited, home, "system")
            upstream = compare.browser_environment(
                inherited, home, "upstream", home, UUID.lower()
            )
            for env in (system, upstream):
                self.assertEqual(env["HOME"], str(home))
                self.assertEqual(env["CFFIXED_USER_HOME"], str(home))
                self.assertEqual(env["FELINAUT_COMPARISON_HOME"], str(home))
                self.assertEqual(env["FELINAUT_AGENT"], "1")
                self.assertEqual(env["KEEP_ME"], "value")
                self.assertNotIn("FELINAUT_COMPARISON_FUTURE", env)
                self.assertNotIn("DYLD_INSERT_LIBRARIES", env)
            self.assertFalse(
                any(
                    key.startswith(("DYLD_", "__XPC_DYLD_", "WEBKIT_"))
                    for key in system
                )
            )
            self.assertNotIn("FELINAUT_EXPECTED_WEBKIT_UUID", system)
            self.assertEqual(upstream["FELINAUT_EXPECTED_WEBKIT_UUID"], UUID)
            self.assertEqual(
                {key for key in upstream if key.startswith(("DYLD_", "__XPC_DYLD_"))},
                set(compare.DYLD_PATHS),
            )
            for key in compare.DYLD_PATHS:
                self.assertEqual(upstream[key], str(home))
            self.assertEqual(upstream["WEBKIT_UNSET_DYLD_FRAMEWORK_PATH"], "YES")
            self.assertEqual(compare.sanitized_environment(upstream)["HOME"], str(home))
        self.assertEqual(inherited, original)

    def test_invalid_environment_policies_are_rejected(self):
        with compare.private_home() as home:
            cases = (
                ("both", None, None),
                ("system", home, UUID),
                ("system", None, UUID),
                ("upstream", None, UUID),
                ("upstream", Path("relative"), UUID),
                ("upstream", home, None),
            )
            for engine, products, uuid in cases:
                with (
                    self.subTest(engine=engine, products=products, uuid=uuid),
                    self.assertRaises(compare.ComparisonError),
                ):
                    compare.browser_environment({}, home, engine, products, uuid)

    def test_private_homes_are_canonical_marked_distinct_and_removed_on_error(self):
        homes = []
        with self.assertRaisesRegex(RuntimeError, "test failure"):
            with compare.private_home() as a, compare.private_home() as b:
                homes.extend((a, b))
                self.assertNotEqual(a, b)
                for home in homes:
                    self.assertEqual(home, home.resolve())
                    self.assertEqual(home.parent, Path("/tmp").resolve())
                    self.assertEqual(stat.S_IMODE(home.stat().st_mode), 0o700)
                    self.assertEqual(
                        (home / ".felinaut-comparison").read_bytes(), compare.MARKER
                    )
                    self.assertLess(
                        len(
                            os.fsencode(
                                str(home)
                                + "/Library/Application Support/Felifox/agent.sock"
                            )
                        ),
                        104,
                    )
                raise RuntimeError("test failure")
        self.assertTrue(all(not home.exists() for home in homes))

    def test_bad_home_mode_marker_or_noncanonical_path_is_rejected(self):
        with compare.private_home() as home:
            home.chmod(0o755)
            with self.assertRaises(compare.ComparisonError):
                compare.validate_home(home)
            home.chmod(0o700)
            marker = home / ".felinaut-comparison"
            marker.write_bytes(compare.MARKER + b"extra")
            with self.assertRaises(compare.ComparisonError):
                compare.validate_home(home)
            marker.write_bytes(compare.MARKER)
            with self.assertRaises(compare.ComparisonError):
                compare.validate_home(home / ".." / home.name)

    def test_budget_uses_one_deadline_not_a_fresh_timeout_per_step(self):
        with patch.object(compare.time, "monotonic", side_effect=[100, 150, 164, 166]):
            budget = compare.Budget()
            self.assertEqual(budget.timeout(20), 15)
            self.assertEqual(budget.timeout(), 1)
            with self.assertRaisesRegex(compare.ComparisonError, "budget"):
                budget.timeout()

    def test_json_and_storage_errors_are_actionable(self):
        with self.assertRaisesRegex(compare.ComparisonError, "malformed JSON"):
            compare.ctl_json(Mock(return_value="not JSON"), "backend")
        for value in ("another engine", {"unexpected": True}, False):
            with (
                self.subTest(value=value),
                self.assertRaisesRegex(compare.ComparisonError, "isolation"),
            ):
                compare.verify_storage(Mock(return_value=json.dumps(value)), None)
        compare.verify_storage(Mock(return_value='{"value":null}'), None)
        compare.verify_storage(
            Mock(return_value='{"value":"system-sentinel"}'), "system-sentinel"
        )


class ProductsTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.products = self.root / "Release"
        self.products.mkdir()
        for name in compare.FRAMEWORKS:
            framework = self.products / f"{name}.framework"
            version = framework / "Versions/A"
            executable(version / name)
            (version / "Resources").mkdir()
            (version / "Resources/Info.plist").write_bytes(plistlib.dumps({}))
            (framework / "Versions/Current").symlink_to("A")
            (framework / name).symlink_to(f"Versions/Current/{name}")
            (framework / "Resources").symlink_to("Versions/Current/Resources")
        self.xpcs = self.products / "WebKit.framework/Versions/A/XPCServices"
        self.xpcs.mkdir()
        (self.products / "WebKit.framework/XPCServices").symlink_to(
            "Versions/Current/XPCServices"
        )
        for service in compare.SERVICES:
            contents = self.xpcs / f"com.apple.WebKit.{service}.xpc/Contents"
            executable(contents / "MacOS/actual-executable-name")
            (contents / "Info.plist").write_bytes(
                plistlib.dumps({"CFBundleExecutable": "actual-executable-name"})
            )
        self.metadata = (
            self.xpcs / "com.apple.WebKit.WebContent.xpc/Contents/Info.plist"
        )
        self.run = self.enterContext(
            patch.object(
                compare.smoke, "run", return_value=f"UUID: {UUID} (arm64) /binary"
            )
        )

    def validate(self):
        return compare.validate_products(self.products, "arm64", {}, compare.Budget())

    def test_complete_tree_reads_actual_plist_executable_and_host_uuid_without_mutation(
        self,
    ):
        before = sorted(
            (str(p.relative_to(self.products)), p.lstat().st_mtime_ns)
            for p in self.products.rglob("*")
        )
        self.assertEqual(self.validate(), (self.products, UUID))
        self.assertEqual(
            self.run.call_count, len(compare.FRAMEWORKS) + len(compare.SERVICES)
        )
        for call in self.run.call_args_list:
            self.assertEqual(call.args[:3], ("/usr/bin/xcrun", "dwarfdump", "--uuid"))
            self.assertGreater(call.kwargs["timeout"], 0)
            self.assertLessEqual(call.kwargs["timeout"], 12)
        after = sorted(
            (str(p.relative_to(self.products)), p.lstat().st_mtime_ns)
            for p in self.products.rglob("*")
        )
        self.assertEqual(before, after)

    def test_missing_products_missing_binary_and_missing_architecture_fail(self):
        with self.assertRaisesRegex(compare.ComparisonError, "Missing upstream"):
            compare.validate_products(
                self.root / "missing", "arm64", {}, compare.Budget()
            )
        self.run.return_value = f"UUID: {UUID} (x86_64) /binary"
        with self.assertRaisesRegex(compare.ComparisonError, "arm64 UUID"):
            self.validate()
        (self.products / "WebKit.framework/Versions/A/WebKit").unlink()
        with self.assertRaisesRegex(compare.ComparisonError, "Missing/broken"):
            self.validate()

    def test_unusable_or_broken_framework_symlink_fails_before_launch(self):
        link = self.products / "WebKit.framework/WebKit"
        link.unlink()
        link.write_text("not a symlink")
        with self.assertRaisesRegex(compare.ComparisonError, "symlink missing"):
            self.validate()
        link.unlink()
        link.symlink_to("Versions/missing/WebKit")
        with self.assertRaisesRegex(compare.ComparisonError, "Missing/broken"):
            self.validate()

    def test_required_runtime_symlink_cannot_escape_products(self):
        outside = executable(self.root / "outside")
        link = self.products / "WebKit.framework/WebKit"
        link.unlink()
        link.symlink_to(outside)
        with self.assertRaisesRegex(compare.ComparisonError, "escapes products"):
            self.validate()

    def test_xpc_plist_missing_malformed_or_traversal_fails(self):
        for value in ("../outside", "", "..", "/bin/sh", ["wrong type"]):
            self.metadata.write_bytes(plistlib.dumps({"CFBundleExecutable": value}))
            with self.subTest(value=value), self.assertRaises(compare.ComparisonError):
                self.validate()
        self.metadata.write_bytes(b"not a plist")
        with self.assertRaisesRegex(compare.ComparisonError, "Malformed XPC"):
            self.validate()
        self.metadata.unlink()
        with self.assertRaisesRegex(compare.ComparisonError, "Missing/broken"):
            self.validate()

    def test_missing_xpc_binary_or_executable_permission_fails(self):
        binary = self.metadata.parent / "MacOS/actual-executable-name"
        binary.chmod(0o600)
        with self.assertRaisesRegex(compare.ComparisonError, "Missing executable"):
            self.validate()
        binary.unlink()
        with self.assertRaisesRegex(compare.ComparisonError, "Missing/broken"):
            self.validate()


class WorkflowTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.output = self.root / "target/output"
        self.output.mkdir(parents=True, mode=0o700)
        for name in ("felinaut", "felinaut-ctl"):
            executable(self.root / "target/debug" / name)
        self.processes = []
        self.environments = []
        self.commands = []
        self.stores = {}
        self.shared_store = False
        self.fail = None
        self.server_count = 0
        self.server_live = False
        self.wrong_source = False
        self.change_uuid = False
        self.backend_calls = {}
        self.launch_failure = False
        self.stuck_first = False
        self.signals_during_cleanup = False
        self.manifest = engine_manifest()
        self.manifest_calls = []

    @contextlib.contextmanager
    def server(self):
        self.server_count += 1
        self.server_live = True
        try:
            yield "http://127.0.0.1:12345/"
        finally:
            self.server_live = False

    def launch(self, command, *, env, stdout, stderr):
        self.assertTrue(self.server_live)
        self.assertEqual(command, [str(self.root / "target/debug/felinaut")])
        compare.validate_home(Path(env["HOME"]))
        self.environments.append(env)
        if self.launch_failure and len(self.environments) == 2:
            raise OSError("deliberate second launch failure")
        process = Mock(pid=123 + len(self.processes), returncode=0)
        process.poll.return_value = None
        process.log = stdout
        index = len(self.processes)

        def wait(*, timeout):
            if self.signals_during_cleanup:
                for _ in range(2):
                    signal.raise_signal(signal.SIGINT)
                    signal.raise_signal(signal.SIGTERM)
            if self.stuck_first and index == 0:
                raise subprocess.TimeoutExpired("owned-root", timeout)
            process.poll.return_value = 0
            return 0

        process.wait.side_effect = wait
        self.processes.append(process)
        return process

    def fake_run(self, executable, *args, env, timeout):
        if args == ("--list-engines",):
            self.assertFalse(self.server_live)
            self.assertGreater(timeout, 0)
            self.assertLessEqual(timeout, 12)
            self.assertEqual(env["HOME"], "/dev/null")
            self.assertEqual(env["FELINAUT_AGENT"], "0")
            self.manifest_calls.append((executable, env, timeout))
            return json.dumps(self.manifest)
        self.assertTrue(self.server_live)
        self.assertGreater(timeout, 0)
        self.assertLessEqual(timeout, 12)
        self.assertFalse(
            any(key.startswith(("DYLD_", "__XPC_DYLD_", "WEBKIT_")) for key in env)
        )
        self.commands.append((env["HOME"], args))
        if self.fail and args[0] == "--native":
            raise self.fail
        home = env["HOME"]
        selected = next(
            item["FELINAUT_COMPARISON"]
            for item in self.environments
            if item["HOME"] == home
        )
        store = "shared" if self.shared_store else home
        if args == ("backend",):
            self.backend_calls[home] = self.backend_calls.get(home, 0) + 1
            info = copy.deepcopy(BACKEND)
            if selected == "system":
                info["runtime_identity"] = {
                    "source": "system-shared-cache",
                    "image_uuid": SYSTEM_UUID,
                }
            if self.wrong_source:
                info["runtime_identity"]["source"] = "unverified"
            if self.change_uuid and self.backend_calls[home] > 1:
                info["runtime_identity"]["image_uuid"] = (
                    "00000000-0000-0000-0000-000000000000"
                )
            return json.dumps(info)
        if args == ("tabs",):
            return json.dumps([{"url": "http://127.0.0.1:12345/", "loading": False}])
        if args == ("snapshot",):
            return '@e0 textbox "Test input" = ""\n@e1 button "Verify input"'
        if args[0] == "eval":
            if args[1].startswith("({value: localStorage.getItem"):
                return json.dumps({"value": self.stores.get(store)})
            if args[1].startswith("localStorage.setItem"):
                self.stores[store] = json.loads(args[1].split(", ", 1)[1][:-1])
                return "null"
            return json.dumps(
                {
                    "text": "Native input verified: Hello from Felinaut",
                    "trusted": "true",
                }
            )
        if args[0] == "screenshot":
            Path(args[1]).write_bytes(PNG)
        return ""

    @contextlib.contextmanager
    def harness(self):
        with (
            patch.object(compare, "ROOT", self.root),
            patch.object(compare.platform, "machine", return_value="arm64"),
            patch.object(compare, "image_uuid", return_value=UUID),
            patch.object(
                compare, "validate_products", return_value=(self.root, UUID)
            ) as products,
            patch.object(compare.smoke, "fixture_server", self.server),
            patch.object(compare.smoke, "run", side_effect=self.fake_run),
            patch.object(compare.subprocess, "Popen", side_effect=self.launch),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            yield products

    def assert_cleaned(self):
        self.assertFalse(self.server_live)
        for env in self.environments:
            self.assertFalse(Path(env["HOME"]).exists())
        for process in self.processes:
            process.terminate.assert_called_once_with()
            process.wait.assert_called_once_with(timeout=2)
            self.assertTrue(process.log.closed)

    def test_both_runs_share_one_origin_but_not_homes_loader_env_or_storage(self):
        with self.harness():
            report = compare.compare(self.output, "both", True, False)
        self.assert_cleaned()
        self.assertEqual(self.server_count, 1)
        self.assertEqual(len(self.stores), 2)
        self.assertEqual(len(set(self.stores.values())), 2)
        self.assertTrue(report["cross_engine_storage_isolation"])
        self.assertEqual(set(report["engines"]), {"system", "upstream"})
        self.assertEqual(report["cleanup_scope"], "owned-browser-roots-only")
        self.assertEqual(report["webkit_helper_lifecycle"], "unverified")
        self.assertNotEqual(self.environments[0]["HOME"], self.environments[1]["HOME"])
        self.assertNotIn("DYLD_FRAMEWORK_PATH", self.environments[0])
        self.assertEqual(self.environments[1]["DYLD_FRAMEWORK_PATH"], str(self.root))
        self.assertEqual(json.loads((self.output / "result.json").read_text()), report)
        for engine in ("system", "upstream"):
            self.assertEqual((self.output / f"{engine}-page.png").read_bytes(), PNG)
        for home in self.stores:
            commands = [args for where, args in self.commands if where == home]
            self.assertIn(("navigate", "http://127.0.0.1:12345/"), commands)
            self.assertIn(("--native", "type", "@e0", "Hello from Felinaut"), commands)
            self.assertIn(("--native", "click", "@e1"), commands)

    def test_system_alone_does_not_read_upstream_products(self):
        self.manifest = engine_manifest(upstream=False)
        with self.harness() as products:
            report = compare.compare(self.output, "system", True, False)
        products.assert_not_called()
        self.assertFalse(report["cross_engine_storage_isolation"])
        self.assert_cleaned()

    def test_excluded_or_malformed_manifest_fails_before_homes_and_browser_launch(self):
        for manifest, selected in (
            (engine_manifest(upstream=False), "upstream"),
            (engine_manifest(upstream=False), "both"),
            (engine_manifest(system=False), "system"),
            (engine_manifest(system=False), "both"),
            ({"schema_version": 0}, "system"),
        ):
            with self.subTest(manifest=manifest, selected=selected):
                self.manifest = manifest
                with (
                    self.harness() as products,
                    patch.object(compare, "private_home") as home,
                    patch.object(compare, "compile_helper") as compiler,
                    self.assertRaisesRegex(compare.ComparisonError, "[Rr]ebuild:"),
                ):
                    compare.compare(self.output, selected, True, True)
                home.assert_not_called()
                products.assert_not_called()
                compiler.assert_not_called()
                self.assertEqual(self.processes, [])
                self.assertEqual(self.server_count, 0)

    def test_default_smoke_requires_both_adapters_without_fallback(self):
        self.manifest = engine_manifest(upstream=False)
        stderr = io.StringIO()
        with (
            self.harness() as products,
            patch.object(compare.sys, "platform", "darwin"),
            patch.object(compare, "create_output", return_value=self.output),
            patch.object(compare, "choose_engine") as choose,
            contextlib.redirect_stderr(stderr),
        ):
            self.assertEqual(compare.main(["--smoke"]), 1)
        choose.assert_not_called()
        products.assert_not_called()
        self.assertEqual(self.processes, [])
        self.assertEqual(self.server_count, 0)
        self.assertIn("--features engine-webkit-upstream", stderr.getvalue())

    def test_upstream_only_build_runs_only_upstream(self):
        self.manifest = engine_manifest(system=False)
        with self.harness() as products:
            report = compare.compare(self.output, "upstream", True, False)
        self.assertEqual(set(report["engines"]), {"upstream"})
        products.assert_called_once()
        self.assertEqual(len(self.processes), 1)
        self.assert_cleaned()

    def test_shared_origin_store_leak_is_detected_and_both_children_cleaned(self):
        self.shared_store = True
        with (
            self.harness(),
            self.assertRaisesRegex(compare.ComparisonError, "isolation"),
        ):
            compare.compare(self.output, "both", True, False)
        self.assert_cleaned()
        self.assertEqual(len(self.processes), 2)
        self.assertFalse((self.output / "result.json").exists())

    def test_wrong_runtime_fails_before_navigate_and_never_falls_back(self):
        self.wrong_source = True
        with (
            self.harness(),
            self.assertRaisesRegex(compare.ComparisonError, "no fallback"),
        ):
            compare.compare(self.output, "upstream", True, False)
        self.assert_cleaned()
        self.assertFalse(any(args[0] == "navigate" for _, args in self.commands))
        self.assertEqual(len(self.processes), 1)

    def test_runtime_change_after_navigation_fails(self):
        self.change_uuid = True
        with self.harness(), self.assertRaisesRegex(compare.ComparisonError, "changed"):
            compare.compare(self.output, "system", True, False)
        self.assert_cleaned()

    def test_action_error_timeout_and_interrupt_cleanup(self):
        for error in (
            compare.ComparisonError("deliberate action failure"),
            compare.smoke.SmokeError("control request timed out"),
            KeyboardInterrupt(),
        ):
            with self.subTest(error=type(error).__name__):
                self.processes.clear()
                self.environments.clear()
                self.stores.clear()
                self.fail = error
                with self.harness(), self.assertRaises(type(error)):
                    compare.compare(self.output, "both", True, False)
                self.assert_cleaned()

    def test_second_launch_failure_also_cleans_first_process_and_both_homes(self):
        self.launch_failure = True
        with self.harness(), self.assertRaisesRegex(OSError, "second launch"):
            compare.compare(self.output, "both", True, False)
        self.assert_cleaned()
        self.assertEqual(len(self.environments), 2)
        self.assertEqual(len(self.processes), 1)

    def test_missing_prebuilt_binary_never_launches(self):
        (self.root / "target/debug/felinaut").unlink()
        with (
            self.harness(),
            self.assertRaisesRegex(
                compare.ComparisonError, "cargo build --locked --bins"
            ),
        ):
            compare.compare(self.output, "both", True, False)
        self.assertEqual(self.processes, [])
        self.assertEqual(self.server_count, 0)

    def test_capture_reuses_window_only_helper_per_owned_pid(self):
        with (
            self.harness(),
            patch.object(compare, "compile_helper") as compile_helper,
            patch.object(
                compare.smoke,
                "capture_window",
                side_effect=lambda process, helper, image, env: image.write_bytes(PNG),
            ) as capture,
        ):
            report = compare.compare(self.output, "both", True, True)
        compile_helper.assert_called_once()
        self.assertEqual(compile_helper.call_args.args[0], "backend_window.swift")
        self.assertTrue(report["window_capture"])
        self.assertEqual(capture.call_count, 2)
        self.assertEqual(
            [item.args[0] for item in capture.call_args_list], self.processes
        )
        for item in capture.call_args_list:
            image = item.args[2]
            self.assertEqual(image.name, "capture.png")
            self.assertEqual(image.parent.parent, self.output)
            self.assertFalse(image.parent.exists())
        for engine in ("system", "upstream"):
            self.assertEqual((self.output / f"{engine}-window.png").read_bytes(), PNG)
        self.assert_cleaned()

    def test_repeated_signals_during_interrupted_cleanup_do_not_skip_roots(self):
        self.signals_during_cleanup = True
        with contextlib.ExitStack() as handlers:
            for signum in (signal.SIGINT, signal.SIGTERM):
                previous = signal.signal(signum, compare.smoke.interrupt)
                handlers.callback(signal.signal, signum, previous)
            with (
                self.harness(),
                patch.object(compare.time, "sleep", side_effect=KeyboardInterrupt),
                self.assertRaises(KeyboardInterrupt),
            ):
                compare.compare(self.output, "both", False, False)
            for signum in (signal.SIGINT, signal.SIGTERM):
                self.assertIs(signal.getsignal(signum), compare.smoke.interrupt)
        self.assert_cleaned()
        self.assertEqual(len(self.processes), 2)
        self.assertFalse((self.output / "result.json").exists())

    def test_failed_root_shutdown_preserves_and_reports_only_its_home(self):
        self.stuck_first = True
        stderr = io.StringIO()
        with (
            self.harness(),
            contextlib.redirect_stderr(stderr),
            self.assertRaisesRegex(compare.ComparisonError, "owned browser root"),
        ):
            compare.compare(self.output, "both", True, False)
        home = Path(self.environments[0]["HOME"])
        self.addCleanup(shutil.rmtree, home)
        compare.validate_home(home)
        self.assertIn(str(home), stderr.getvalue())
        self.assertIn(str(self.processes[0].pid), stderr.getvalue())
        self.assertIn("not confirmed stopped", stderr.getvalue())
        self.assertFalse(Path(self.environments[1]["HOME"]).exists())
        self.processes[0].kill.assert_called_once_with()
        self.assertEqual(
            [call.kwargs["timeout"] for call in self.processes[0].wait.call_args_list],
            [2, 1],
        )
        self.processes[1].terminate.assert_called_once_with()
        self.processes[1].wait.assert_called_once_with(timeout=2)
        self.assertTrue(all(process.log.closed for process in self.processes))
        self.assertFalse(self.server_live)
        self.assertFalse((self.output / "result.json").exists())

    def test_interrupted_failed_cleanup_returns_failure_without_success_claim(self):
        self.stuck_first = True
        stderr = io.StringIO()
        stdout = io.StringIO()
        with (
            self.harness(),
            patch.object(compare.sys, "platform", "darwin"),
            patch.object(compare, "create_output", return_value=self.output),
            patch.object(compare.time, "sleep", side_effect=KeyboardInterrupt),
            contextlib.redirect_stderr(stderr),
            contextlib.redirect_stdout(stdout),
        ):
            self.assertEqual(compare.main(["--engine", "both"]), 1)
        home = Path(self.environments[0]["HOME"])
        self.addCleanup(shutil.rmtree, home)
        self.assertTrue(home.exists())
        self.assertIn(str(home), stderr.getvalue())
        self.assertIn("FAIL:", stderr.getvalue())
        self.assertNotIn("cleaned up", stderr.getvalue())
        self.assertNotIn("PASS:", stdout.getvalue())
        self.assertFalse((self.output / "result.json").exists())
        self.assertFalse(self.server_live)

    def test_page_and_window_capture_failures_remove_raw_staging_files(self):
        for window in (False, True):
            for failure in (
                compare.smoke.SmokeError("capture failed"),
                KeyboardInterrupt(),
            ):
                with self.subTest(window=window, failure=type(failure).__name__):
                    self.processes.clear()
                    self.environments.clear()
                    self.stores.clear()
                    original = self.fake_run

                    def raw_capture(image):
                        image.write_bytes(PNG + b"unsanitized capture")
                        raise failure

                    def run(executable, *args, env, timeout):
                        if not window and args[0] == "screenshot":
                            raw_capture(Path(args[1]))
                        return original(executable, *args, env=env, timeout=timeout)

                    with (
                        self.harness(),
                        patch.object(compare.smoke, "run", side_effect=run),
                        patch.object(compare, "compile_helper"),
                        patch.object(
                            compare.smoke,
                            "capture_window",
                            side_effect=lambda process, helper, image, env: raw_capture(
                                image
                            ),
                        ),
                        self.assertRaises(type(failure)),
                    ):
                        compare.compare(self.output, "system", True, window)
                    destination = self.output / (
                        "system-window.png" if window else "system-page.png"
                    )
                    self.assertFalse(destination.exists())
                    self.assertEqual(list(self.output.glob(".capture-*")), [])
                    self.assert_cleaned()

    def test_interactive_mode_waits_for_owned_windows_instead_of_smoke_exit(self):
        def closed_after_verification(delay):
            for process in self.processes:
                process.poll.return_value = 0

        with (
            self.harness(),
            patch.object(
                compare.time, "sleep", side_effect=closed_after_verification
            ) as sleep,
        ):
            compare.compare(self.output, "system", False, False)
        sleep.assert_called_once_with(0.2)
        self.processes[0].terminate.assert_not_called()
        self.assertFalse(self.server_live)
        self.assertFalse(Path(self.environments[0]["HOME"]).exists())


class CaptureTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.output = Path(temporary.name)
        self.destination = self.output / "page.png"
        self.raw = PNG[:-12] + chunk(b"tEXt", b"Author\0private") + PNG[-12:]

    def test_only_sanitized_complete_png_is_published(self):
        staged = []
        clean_png = compare.smoke.clean_png

        def capture(image):
            staged.append(image)
            self.assertFalse(self.destination.exists())
            self.assertEqual(image.parent.parent, self.output)
            self.assertEqual(stat.S_IMODE(image.parent.stat().st_mode), 0o700)
            image.write_bytes(self.raw)

        def sanitize(image):
            self.assertFalse(self.destination.exists())
            self.assertEqual(image.read_bytes(), self.raw)
            clean_png(image)
            self.assertFalse(self.destination.exists())

        with patch.object(compare.smoke, "clean_png", side_effect=sanitize):
            compare.publish_capture(self.destination, capture)
        self.assertEqual(self.destination.read_bytes(), PNG)
        self.assertFalse(staged[0].parent.exists())
        self.assertEqual(list(self.output.iterdir()), [self.destination])

    def test_missing_or_malformed_capture_is_not_published(self):
        for contents in (None, b"", b"not a PNG", PNG[:-1]):
            with self.subTest(contents=contents):

                def capture(image):
                    if contents is not None:
                        image.write_bytes(contents)

                with self.assertRaises((OSError, compare.smoke.SmokeError)):
                    compare.publish_capture(self.destination, capture)
                self.assertEqual(list(self.output.iterdir()), [])

    def test_sanitation_or_publication_failure_discards_staging(self):
        for publish in (False, True):
            for failure in (OSError("write failed"), KeyboardInterrupt()):
                with self.subTest(publish=publish, failure=type(failure).__name__):

                    def capture(image):
                        image.write_bytes(self.raw)

                    def fail_sanitation(image):
                        image.write_bytes(b"partially rewritten PNG")
                        raise failure

                    target = Path if publish else compare.smoke
                    method = "replace" if publish else "clean_png"
                    effect = failure if publish else fail_sanitation
                    with (
                        patch.object(target, method, side_effect=effect),
                        self.assertRaises(type(failure)),
                    ):
                        compare.publish_capture(self.destination, capture)
                    self.assertEqual(list(self.output.iterdir()), [])

    def test_failure_does_not_replace_an_existing_artifact(self):
        self.destination.write_bytes(PNG)

        def capture(image):
            image.write_bytes(b"invalid")

        with self.assertRaises(compare.smoke.SmokeError):
            compare.publish_capture(self.destination, capture)
        self.assertEqual(self.destination.read_bytes(), PNG)
        self.assertEqual(list(self.output.iterdir()), [self.destination])


class EntryAndCleanupTests(unittest.TestCase):
    def test_import_has_no_subprocess_or_filesystem_side_effects(self):
        spec = importlib.util.spec_from_file_location(
            "comparison_import_check", compare.__file__
        )
        with (
            patch.object(compare.subprocess, "Popen") as launch,
            patch.object(compare.smoke, "run") as run,
            patch.object(compare.tempfile, "mkdtemp") as mkdir,
        ):
            spec.loader.exec_module(importlib.util.module_from_spec(spec))
        launch.assert_not_called()
        run.assert_not_called()
        mkdir.assert_not_called()

    def test_output_is_new_private_target_only_and_never_overwrites(self):
        with (
            tempfile.TemporaryDirectory() as name,
            patch.object(compare, "ROOT", Path(name)),
        ):
            output = compare.create_output()
            self.assertEqual(output.parent, Path(name).resolve() / "target")
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o700)
            with self.assertRaises(FileExistsError):
                compare.create_output(output)
            with self.assertRaisesRegex(compare.ComparisonError, "under target"):
                compare.create_output(Path(name) / "outside")

    def test_smoke_default_skips_picker_and_passes_explicit_products(self):
        with (
            patch.object(compare.sys, "platform", "darwin"),
            patch.object(
                compare, "create_output", return_value=Path("/private/output")
            ),
            patch.object(compare, "choose_engine") as choose,
            patch.object(compare, "compare") as run,
            contextlib.redirect_stdout(io.StringIO()),
        ):
            self.assertEqual(
                compare.main(["--smoke", "--products", "/trusted/Release"]), 0
            )
        choose.assert_not_called()
        run.assert_called_once_with(
            Path("/private/output"), "both", True, False, Path("/trusted/Release")
        )

    def test_interactive_cancel_does_not_launch(self):
        with (
            patch.object(compare.sys, "platform", "darwin"),
            patch.object(
                compare, "create_output", return_value=Path("/private/output")
            ),
            patch.object(compare, "built_modes", return_value=("system",)),
            patch.object(compare, "choose_engine", return_value=None),
            patch.object(compare, "compare") as run,
            contextlib.redirect_stdout(io.StringIO()),
        ):
            self.assertEqual(compare.main([]), 0)
        run.assert_not_called()

    def test_platform_is_checked_before_creating_output(self):
        with (
            patch.object(compare.sys, "platform", "linux"),
            patch.object(compare, "create_output") as output,
            contextlib.redirect_stderr(io.StringIO()),
            self.assertRaises(SystemExit),
        ):
            compare.main(["--smoke"])
        output.assert_not_called()

    def test_picker_protocol_wires_supported_modes_and_rejects_excluded_results(self):
        with tempfile.TemporaryDirectory() as name:
            output = Path(name)
            environment = {"PATH": "/usr/bin:/bin"}
            for supported in (
                ("system",),
                ("upstream",),
                ("system", "upstream", "both"),
            ):
                for choice in (
                    "system",
                    "upstream",
                    "both",
                    "cancel",
                    "wpe",
                    "unexpected",
                ):
                    with (
                        self.subTest(supported=supported, choice=choice),
                        patch.object(compare, "compile_helper") as compiler,
                        patch.object(compare.smoke, "run", return_value=choice) as run,
                    ):
                        if choice not in (*supported, "cancel"):
                            with self.assertRaisesRegex(
                                compare.ComparisonError, "unknown or excluded"
                            ):
                                compare.choose_engine(output, environment, supported)
                        else:
                            self.assertEqual(
                                compare.choose_engine(output, environment, supported),
                                None if choice == "cancel" else choice,
                            )
                        compiler.assert_called_once_with(
                            "choose_webkit.swift", output / "choose-webkit", environment
                        )
                        run.assert_called_once_with(
                            output / "choose-webkit",
                            "--supported-modes",
                            *supported,
                            env=environment,
                            timeout=None,
                        )
            with patch.object(compare.smoke, "run") as run:
                compare.compile_helper("choose_webkit.swift", output / "picker", {})
            self.assertEqual(run.call_args.kwargs["timeout"], 60)

    def test_invalid_supported_picker_modes_never_compile_or_run_helper(self):
        for supported in (
            (),
            ("both",),
            ("system", "both"),
            ("system", "upstream"),
            ("system", "system"),
            ("wpe",),
            ("future",),
        ):
            with (
                self.subTest(supported=supported),
                patch.object(compare, "compile_helper") as compiler,
                patch.object(compare.smoke, "run") as run,
                self.assertRaises(compare.ComparisonError),
            ):
                compare.choose_engine(Path("/unused/output"), {}, supported)
            compiler.assert_not_called()
            run.assert_not_called()

    def test_main_passes_manifest_supported_modes_to_native_picker(self):
        for supported, selected in (
            (("system",), "system"),
            (("upstream",), "upstream"),
            (("system", "upstream", "both"), "both"),
        ):
            with (
                self.subTest(supported=supported),
                patch.object(compare.sys, "platform", "darwin"),
                patch.object(
                    compare, "create_output", return_value=Path("/private/output")
                ),
                patch.object(
                    compare, "built_modes", return_value=supported
                ) as manifest,
                patch.object(compare, "choose_engine", return_value=selected) as choose,
                patch.object(compare, "compare") as run,
                contextlib.redirect_stdout(io.StringIO()),
            ):
                self.assertEqual(compare.main([]), 0)
            manifest.assert_called_once()
            self.assertEqual(choose.call_args.args[0], Path("/private/output"))
            self.assertEqual(choose.call_args.args[2], supported)
            run.assert_called_once_with(
                Path("/private/output"),
                selected,
                False,
                False,
                compare.DEFAULT_PRODUCTS,
            )

    def test_interrupt_message_does_not_assert_successful_cleanup(self):
        stderr = io.StringIO()
        with (
            patch.object(compare.sys, "platform", "darwin"),
            patch.object(compare, "create_output", side_effect=KeyboardInterrupt),
            contextlib.redirect_stderr(stderr),
        ):
            self.assertEqual(compare.main(["--smoke"]), 130)
        self.assertIn("cleanup attempted", stderr.getvalue())
        self.assertIn("WebKit helper lifecycle is unverified", stderr.getvalue())
        self.assertNotIn("cleaned up", stderr.getvalue())

    def test_repeated_signals_do_not_interrupt_termination_or_kill_escalation(self):
        first, second = Mock(), Mock()
        for process in (first, second):
            process.poll.return_value = None
        first.wait.side_effect = [subprocess.TimeoutExpired("owned", 2), 0]

        def repeated_signals():
            for _ in range(2):
                signal.raise_signal(signal.SIGINT)
                signal.raise_signal(signal.SIGTERM)

        first.terminate.side_effect = repeated_signals
        first.kill.side_effect = repeated_signals
        second.terminate.side_effect = repeated_signals
        with contextlib.ExitStack() as handlers:
            for signum in (signal.SIGINT, signal.SIGTERM):
                previous = signal.signal(signum, compare.smoke.interrupt)
                handlers.callback(signal.signal, signum, previous)
            try:
                compare.stop_owned([first, second])
            except KeyboardInterrupt:
                self.fail("Repeated signals interrupted owned-root cleanup")
            for signum in (signal.SIGINT, signal.SIGTERM):
                self.assertIs(signal.getsignal(signum), compare.smoke.interrupt)
        first.kill.assert_called_once_with()
        self.assertEqual(
            [call.kwargs["timeout"] for call in first.wait.call_args_list], [2, 1]
        )
        second.wait.assert_called_once_with(timeout=2)

    def test_repeated_signals_do_not_interrupt_home_removal(self):
        rmtree = shutil.rmtree
        removed = []

        def remove(home):
            for _ in range(2):
                signal.raise_signal(signal.SIGINT)
                signal.raise_signal(signal.SIGTERM)
            rmtree(home)
            removed.append(home)

        with contextlib.ExitStack() as handlers:
            for signum in (signal.SIGINT, signal.SIGTERM):
                previous = signal.signal(signum, compare.smoke.interrupt)
                handlers.callback(signal.signal, signum, previous)
            with patch.object(compare.shutil, "rmtree", side_effect=remove):
                try:
                    with compare.private_home() as home:
                        pass
                except KeyboardInterrupt:
                    rmtree(home)
                    self.fail("Repeated signals interrupted temporary HOME removal")
            for signum in (signal.SIGINT, signal.SIGTERM):
                self.assertIs(signal.getsignal(signum), compare.smoke.interrupt)
        self.assertEqual(removed, [home])
        self.assertFalse(home.exists())

    def test_poll_error_does_not_skip_stopping_or_waiting_for_other_roots(self):
        unknown, other = Mock(), Mock()
        unknown.poll.side_effect = OSError("unknown root state")
        other.poll.return_value = None
        with self.assertRaisesRegex(compare.ComparisonError, "owned browser root"):
            compare.stop_owned([unknown, other])
        unknown.wait.assert_called_once_with(timeout=2)
        other.terminate.assert_called_once_with()
        other.wait.assert_called_once_with(timeout=2)

    def test_unknown_root_poll_preserves_home_without_automatic_finalizer(self):
        root = Mock(pid=123)
        root.poll.side_effect = OSError("cannot determine root state")
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            with compare.private_home([root]) as home:
                pass
        self.addCleanup(shutil.rmtree, home)
        compare.validate_home(home)
        self.assertIn(str(home), stderr.getvalue())
        self.assertIn("123 not confirmed stopped", stderr.getvalue())

    def test_cleanup_escalates_only_owned_children_with_short_bounded_waits(self):
        process = Mock()
        process.poll.return_value = None
        process.wait.side_effect = [subprocess.TimeoutExpired("owned", 2), 0]
        compare.stop_owned([process])
        process.terminate.assert_called_once()
        process.kill.assert_called_once()
        self.assertEqual(
            [call.kwargs["timeout"] for call in process.wait.call_args_list], [2, 1]
        )

    def test_cleanup_failure_for_one_child_does_not_skip_other_owned_children(self):
        stuck, other = Mock(), Mock()
        for process in (stuck, other):
            process.poll.return_value = None
        stuck.wait.side_effect = subprocess.TimeoutExpired("stuck", 1)
        with self.assertRaisesRegex(compare.ComparisonError, "stop/reap"):
            compare.stop_owned([stuck, other])
        stuck.kill.assert_called_once()
        other.terminate.assert_called_once()
        other.wait.assert_called_once_with(timeout=2)


if __name__ == "__main__":
    unittest.main()
