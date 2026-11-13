"""Offline runner tests: no GUI, compiler, engine or real subprocess is launched."""

import errno
import http.client
import io
import json
import os
import shutil
import signal
import struct
import subprocess
import tempfile
import unittest
import zlib
from contextlib import contextmanager, redirect_stderr, redirect_stdout
from pathlib import Path
from unittest.mock import Mock, patch
from urllib.parse import urlsplit

import run_probe as runner


class Root:
    def __init__(self, code=None):
        self.pid = 43210
        self.returncode = code
        self.terminated = False
        self.killed = False

    def poll(self):
        return self.returncode

    def terminate(self):
        self.terminated = True
        self.returncode = -15

    def kill(self):
        self.killed = True
        self.returncode = -9

    def wait(self, timeout):
        if self.returncode is None:
            raise subprocess.TimeoutExpired("owned-root", timeout)
        return self.returncode


def successful_audit():
    helpers = [
        {
            "pid": 43211 + index,
            "role": role,
            "seatbelt": 1,
            "ready": True,
            "policy_verified": True,
            "exit_evidence": "kqueue_NOTE_EXIT",
            "gaps": [],
        }
        for index, role in enumerate(("renderer", "gpu-process", "utility", "renderer"))
    ]
    return {
        "ok": True,
        "root_pid": 43210,
        "host_launch_count": len(helpers),
        "distinct_starts": len(helpers),
        "record_counts": {"start": 4, "ready": 4, "launch": 4},
        "controls": {"canary_open": True, "allowed_bundle_file": True},
        "roles": {
            role: {
                "count": sum(helper["role"] == role for helper in helpers),
                "policy_verified": True,
                "exits_verified": True,
                "gaps": [],
            }
            for role in ("renderer", "gpu-process", "utility")
        },
        "helpers": helpers,
        "gaps": [],
    }


def native_fixture(markers=runner.MARKERS):
    lines = []
    for helper in successful_audit()["helpers"]:
        pid, role = helper["pid"], helper["role"]
        lines.extend(
            (
                "CEF_HELPER_LAUNCH",
                f"CEF_HELPER_START pid={pid} ppid=43210 role={role} "
                f"subtype=none sandbox={role} seatbelt=1",
                f"CEF_HELPER_SANDBOX_READY pid={pid}",
            )
        )
        if role == "renderer":
            lines.append(f"CEF_RENDERER_CANARY pid={pid} outcome=denied errno=1")
    return "\n".join(lines + list(markers)) + "\n"


def png_fixture():
    def chunk(kind, data):
        return (
            struct.pack(">I", len(data))
            + kind
            + data
            + struct.pack(">I", zlib.crc32(kind + data))
        )

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
        + chunk(b"tEXt", b"Description\0private fixture metadata")
        + chunk(b"IDAT", zlib.compress(b"\0\0\0\0"))
        + chunk(b"IEND", b"")
    )


class RunnerTests(unittest.TestCase):
    def setUp(self):
        artifacts = runner.prepare_runtime.private_directory(
            runner.prepare_runtime.DEFAULT_ROOT
        )
        temporary = tempfile.TemporaryDirectory(prefix=".runner-tests-", dir=artifacts)
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.output = self.base / "output"
        self.output.mkdir(mode=0o700)
        self.fixture = self.base / "fixture.html"
        self.fixture.write_bytes(
            b"<!doctype html><script>document.title='offline fixture'</script>"
        )
        self.coex_fixture = self.base / "coexistence.html"
        self.coex_fixture.write_bytes(
            b"<!doctype html><title>offline coexistence fixture</title>"
        )
        self.profiles = []
        self.addCleanup(self.clean_profiles)
        for name, fixture in (
            ("FIXTURE", self.fixture),
            ("COEX_FIXTURE", self.coex_fixture),
        ):
            fixture_patch = patch.object(runner, name, fixture)
            fixture_patch.start()
            self.addCleanup(fixture_patch.stop)
        for guard in (
            patch.object(
                runner.subprocess,
                "Popen",
                side_effect=AssertionError("real subprocess"),
            ),
            patch.object(
                runner.helper_observer,
                "NativeAPI",
                side_effect=AssertionError("real native API"),
            ),
        ):
            guard.start()
            self.addCleanup(guard.stop)

    def clean_profiles(self):
        for profile in self.profiles:
            if profile.path.is_symlink():
                profile.path.unlink()
            elif profile.path.exists():
                shutil.rmtree(profile.path)

    def profile(self):
        profile = runner.create_profile()
        self.profiles.append(profile)
        return profile

    def test_default_and_build_capture_without_run_are_pure_plans(self):
        for arguments in ([], ["--build"], ["--capture"], ["--build", "--capture"]):
            with (
                self.subTest(arguments=arguments),
                redirect_stdout(io.StringIO()) as output,
            ):
                with (
                    patch.object(
                        runner, "execute", side_effect=AssertionError("execution")
                    ),
                    patch.object(
                        runner,
                        "create_output",
                        side_effect=AssertionError("disk write"),
                    ),
                    patch.object(
                        runner.subprocess,
                        "Popen",
                        side_effect=AssertionError("subprocess"),
                    ),
                    patch.object(
                        runner.bundle_probe,
                        "verify_prepared_sdk",
                        side_effect=AssertionError("SDK access"),
                    ),
                ):
                    self.assertEqual(runner.main(arguments), 0)
                plan = json.loads(output.getvalue())
                self.assertEqual(plan["mode"], "dry-run")
                self.assertEqual(bool(plan["build_commands"]), "--build" in arguments)
                self.assertEqual(plan["capture"], "--capture" in arguments)
        self.assertEqual(list(self.output.iterdir()), [])

    def test_coexistence_without_run_is_a_pure_plan_with_selected_commands(self):
        for build, capture in (
            (False, False),
            (True, False),
            (False, True),
            (True, True),
        ):
            arguments = ["--coexistence"]
            if build:
                arguments.append("--build")
            if capture:
                arguments.append("--capture")
            with (
                self.subTest(build=build, capture=capture),
                redirect_stdout(io.StringIO()) as output,
                patch.object(
                    runner, "execute", side_effect=AssertionError("execution")
                ),
                patch.object(
                    runner, "create_output", side_effect=AssertionError("disk write")
                ),
                patch.object(
                    runner.bundle_probe,
                    "verify_prepared_sdk",
                    side_effect=AssertionError("SDK access"),
                ),
            ):
                self.assertEqual(runner.main(arguments), 0)
            plan = json.loads(output.getvalue())
            self.assertEqual(plan, runner.plan(build, capture, True))
            self.assertEqual(plan["mode"], "dry-run")
            self.assertEqual(plan["probe_mode"], "coexistence")
            self.assertEqual(plan["command"][-1], "--run-coexistence")
            self.assertEqual(plan["capture"], capture)
            self.assertEqual(
                plan["build_commands"], runner.build_commands(True) if build else []
            )
        self.assertEqual(runner.plan()["probe_mode"], "cef")
        self.assertEqual(runner.plan()["command"][-1], "--run-cef")
        self.assertEqual(list(self.output.iterdir()), [])

    def test_coexistence_cli_forwards_mode_only_with_explicit_run(self):
        for build, capture in (
            (False, False),
            (True, False),
            (False, True),
            (True, True),
        ):
            arguments = ["--run", "--coexistence"]
            if build:
                arguments.append("--build")
            if capture:
                arguments.append("--capture")
            with (
                self.subTest(build=build, capture=capture),
                patch.object(
                    runner, "execute", return_value={"success": True}
                ) as execute,
                redirect_stdout(io.StringIO()),
            ):
                self.assertEqual(runner.main(arguments), 0)
            execute.assert_called_once_with(build, capture, True)
        with (
            patch.object(
                runner, "execute", side_effect=runner.ProbeError("coexistence failure")
            ),
            redirect_stderr(io.StringIO()),
        ):
            self.assertEqual(runner.main(["--run", "--coexistence"]), 1)

    def test_parser_rejects_urls_profiles_and_unsafe_flags(self):
        for arguments in (
            ["--url", "https://example.com"],
            ["--home", "/tmp"],
            ["--no-sandbox"],
            ["--single-process"],
            ["--r"],
            ["https://example.com"],
            ["--coexistence", "--url", "http://127.0.0.1:1234/cef"],
            ["--coexistence", "--features", "engine-cef"],
            ["--coexistence", "--run-coexistence"],
        ):
            with self.subTest(arguments=arguments), redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit):
                    runner.main(arguments)

    def test_explicit_run_routes_only_supported_options_and_propagates_failure(self):
        with (
            patch.object(runner, "execute", return_value={"success": True}) as execute,
            redirect_stdout(io.StringIO()),
        ):
            self.assertEqual(runner.main(["--run", "--build", "--capture"]), 0)
        execute.assert_called_once_with(True, True)
        with (
            patch.object(
                runner, "execute", side_effect=runner.ProbeError("fixture failure")
            ),
            redirect_stderr(io.StringIO()),
        ):
            self.assertEqual(runner.main(["--run"]), 1)

    def test_environment_scrubs_all_requested_families_without_mutating_input(self):
        inherited = {
            name: "untrusted"
            for name in (
                "DYLD_LIBRARY_PATH",
                "__XPC_DYLD_INSERT_LIBRARIES",
                "WEBKIT_TEST",
                "CEF_PATH",
                "CHROME_LOG_FILE",
                "FELINAUT_AGENT",
                "FELINAUT_CEF_PROBE_URL",
                "FLATPAK",
                "NIX_CEF_BINARY",
            )
        }
        inherited.update(PATH="/usr/bin", HOME="/original", CARGO_HOME="/cargo")
        before = dict(inherited)
        clean = runner.sanitized_environment(inherited)
        self.assertEqual(
            clean, {"PATH": "/usr/bin", "HOME": "/original", "CARGO_HOME": "/cargo"}
        )
        self.assertEqual(inherited, before)

    def test_profile_and_launch_environment_exact_contract(self):
        profile = self.profile()
        runner.validate_profile(profile.path, fresh=True)
        self.assertEqual(profile.path.parent, Path("/tmp").resolve())
        self.assertTrue(profile.path.name.startswith("felinaut-cef-probe-"))
        self.assertEqual(profile.path.stat().st_mode & 0o777, 0o700)
        self.assertEqual(
            (profile.path / runner.MARKER_NAME).read_bytes(), runner.MARKER
        )
        self.assertEqual(
            (profile.path / runner.MARKER_NAME).stat().st_mode & 0o777, 0o600
        )
        env = runner.launch_environment(
            {"HOME": "/real", "CEF_PATH": "/bad", "FELINAUT_AGENT": "1"},
            profile,
            "http://127.0.0.1:45678/",
        )
        for key in ("HOME", "CFFIXED_USER_HOME", "FELINAUT_CEF_PROBE_HOME"):
            self.assertEqual(env[key], str(profile.path))
        self.assertEqual(env["FELINAUT_CEF_PROBE_URL"], "http://127.0.0.1:45678/")
        self.assertNotIn("CEF_PATH", env)
        self.assertNotIn("FELINAUT_AGENT", env)

    def test_native_environment_is_an_exact_allowlist_not_the_build_environment(self):
        profile = self.profile()
        inherited = {
            "HOME": "/original/home",
            "CFFIXED_USER_HOME": "/wrong",
            "TMPDIR": "/original/tmp/",
            "PATH": "/untrusted/bin",
            "LANG": "en_US.UTF-8",
            "LC_CTYPE": "UTF-8",
            "LC_ALL": "C",
            "LC_MESSAGES": "C",
            "__CF_USER_TEXT_ENCODING": "0x1F5:0:0",
            "AWS_SECRET_ACCESS_KEY": "sentinel-aws",
            "GITHUB_TOKEN": "sentinel-github",
            "SSH_AUTH_SOCK": "/agent",
            "SSLKEYLOGFILE": "/outside/ssl-keys",
            "HTTP_PROXY": "http://outside:1234",
            "XDG_CACHE_HOME": "/outside/cache",
            "LC_UNRECOGNIZED_SECRET": "sentinel",
            "CARGO_HOME": "/cargo",
            "RUSTUP_HOME": "/rustup",
            "DEVELOPER_DIR": "/Xcode",
            "SDKROOT": "/sdk",
            "CEF_PATH": "/wrong/sdk",
            "DYLD_INSERT_LIBRARIES": "/inject",
            "FELINAUT_AGENT": "1",
        }
        before = dict(inherited)
        native = runner.launch_environment(
            inherited, profile, "http://127.0.0.1:45678/"
        )
        self.assertEqual(
            native,
            {
                "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
                "LANG": "en_US.UTF-8",
                "LC_CTYPE": "UTF-8",
                "LC_ALL": "C",
                "LC_MESSAGES": "C",
                "__CF_USER_TEXT_ENCODING": "0x1F5:0:0",
                "HOME": str(profile.path),
                "CFFIXED_USER_HOME": str(profile.path),
                "FELINAUT_CEF_PROBE_HOME": str(profile.path),
                "FELINAUT_CEF_PROBE_URL": "http://127.0.0.1:45678/",
            },
        )
        build = runner.sanitized_environment(inherited)
        self.assertEqual(build["CARGO_HOME"], "/cargo")
        self.assertEqual(build["RUSTUP_HOME"], "/rustup")
        self.assertEqual(build["DEVELOPER_DIR"], "/Xcode")
        self.assertEqual(build["SDKROOT"], "/sdk")
        self.assertNotIn("CEF_PATH", build)
        self.assertEqual(inherited, before)

    def test_empty_native_environment_inherits_no_ambient_values(self):
        profile = self.profile()
        with patch.dict(
            runner.os.environ, {"AWS_SECRET_ACCESS_KEY": "sentinel"}, clear=True
        ):
            env = runner.launch_environment({}, profile, "http://127.0.0.1:45678/")
        self.assertEqual(
            set(env),
            {
                "PATH",
                "HOME",
                "CFFIXED_USER_HOME",
                "FELINAUT_CEF_PROBE_HOME",
                "FELINAUT_CEF_PROBE_URL",
            },
        )

    def test_capture_environment_uses_original_desktop_home_without_credentials(self):
        inherited = {
            "HOME": "/desktop/home",
            "TMPDIR": "/desktop/tmp/",
            "LANG": "en_US.UTF-8",
            "PATH": "/untrusted/bin",
            "CFFIXED_USER_HOME": "/cef/profile",
            "FELINAUT_CEF_PROBE_HOME": "/cef/profile",
            "FELINAUT_CEF_PROBE_URL": "http://127.0.0.1:3/",
            "AWS_SECRET_ACCESS_KEY": "sentinel",
            "SSLKEYLOGFILE": "/outside/keys",
            "DYLD_INSERT_LIBRARIES": "/inject",
            "CARGO_HOME": "/cargo",
        }
        before = dict(inherited)
        self.assertEqual(
            runner.capture_environment(inherited),
            {
                "HOME": "/desktop/home",
                "TMPDIR": "/desktop/tmp/",
                "LANG": "en_US.UTF-8",
                "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
            },
        )
        self.assertEqual(inherited, before)
        self.assertEqual(
            runner.capture_environment({}), {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin"}
        )

    def test_native_canary_environment_uses_explicit_path_not_shell_override(self):
        profile = self.profile()
        canary, _ = runner.create_canary(self.output)
        inherited = {"FELINAUT_CEF_PROBE_CANARY": "/outside/shell-canary"}
        without = runner.launch_environment(
            inherited, profile, "http://127.0.0.1:45678/"
        )
        self.assertNotIn("FELINAUT_CEF_PROBE_CANARY", without)
        with_canary = runner.launch_environment(
            inherited, profile, "http://127.0.0.1:45678/", canary
        )
        self.assertEqual(
            with_canary, {**without, "FELINAUT_CEF_PROBE_CANARY": str(canary)}
        )
        self.assertEqual(
            inherited, {"FELINAUT_CEF_PROBE_CANARY": "/outside/shell-canary"}
        )
        self.assertNotIn(
            "FELINAUT_CEF_PROBE_CANARY", runner.sanitized_environment(inherited)
        )
        self.assertNotIn(
            "FELINAUT_CEF_PROBE_CANARY", runner.capture_environment(inherited)
        )

    def test_canary_is_private_owned_and_returns_stable_file_identity(self):
        path, identity = runner.create_canary(self.output)
        self.assertEqual(path, self.output / "sandbox-canary")
        self.assertFalse(path.is_symlink())
        info = path.stat()
        self.assertEqual(info.st_uid, os.getuid())
        self.assertEqual(info.st_mode & 0o777, 0o600)
        self.assertEqual(path.read_bytes(), runner.CANARY_BYTES)
        self.assertEqual(
            identity, (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns)
        )
        self.assertEqual(runner.validate_canary(path), identity)
        self.assertEqual(runner.validate_canary(path), identity)
        self.assertEqual(list(self.output.iterdir()), [path])

    def test_canary_rejects_empty_changed_truncated_and_appended_contents(self):
        path, _ = runner.create_canary(self.output)
        for content in (
            b"",
            b"wrong fixture\n",
            runner.CANARY_BYTES[:-1],
            runner.CANARY_BYTES + b"extra",
            runner.CANARY_BYTES + b"x" * 256,
        ):
            with self.subTest(content=content):
                path.write_bytes(content)
                with self.assertRaisesRegex(runner.ProbeError, "changed"):
                    runner.validate_canary(path)
        with self.assertRaises(runner.ProbeError):
            runner.validate_canary(self.output)
        path.unlink()
        with self.assertRaises(FileNotFoundError):
            runner.validate_canary(path)

    def test_canary_identity_detects_metadata_change_and_same_bytes_replacement(self):
        path, original = runner.create_canary(self.output)
        info = path.stat()
        os.utime(path, ns=(info.st_atime_ns, info.st_mtime_ns + 1_000_000_000))
        changed = runner.validate_canary(path)
        self.assertEqual(changed[:3], original[:3])
        self.assertNotEqual(changed[3], original[3])
        path.rename(self.output / "original-canary")
        path.write_bytes(runner.CANARY_BYTES)
        replacement = runner.validate_canary(path)
        self.assertNotEqual(replacement[:2], original[:2])
        self.assertEqual(replacement[2], original[2])

    def test_canary_duplicate_creation_does_not_overwrite_existing_file(self):
        path, original = runner.create_canary(self.output)
        with self.assertRaises(FileExistsError):
            runner.create_canary(self.output)
        self.assertEqual(runner.validate_canary(path), original)
        path.write_bytes(b"preserve existing foreign content")
        with self.assertRaises(FileExistsError):
            runner.create_canary(self.output)
        self.assertEqual(path.read_bytes(), b"preserve existing foreign content")

    def test_canary_creation_and_validation_refuse_symlinks_without_touching_target(
        self,
    ):
        target = self.base / "canary-target"
        target.write_bytes(runner.CANARY_BYTES)
        before = target.stat()
        path = self.output / "sandbox-canary"
        path.symlink_to(target)
        with self.assertRaises(FileExistsError):
            runner.create_canary(self.output)
        with self.assertRaises(OSError) as error:
            runner.validate_canary(path)
        self.assertEqual(error.exception.errno, errno.ELOOP)
        self.assertTrue(path.is_symlink())
        self.assertEqual(target.read_bytes(), runner.CANARY_BYTES)
        self.assertEqual(target.stat().st_mtime_ns, before.st_mtime_ns)
        self.assertEqual(target.stat().st_ino, before.st_ino)

    def test_renderer_canaries_accept_exact_denials_for_two_renderers_only(self):
        audit = successful_audit()
        records = (
            "CEF_RENDERER_CANARY pid=43211 outcome=denied errno=1",
            "CEF_RENDERER_CANARY pid=43214 outcome=denied errno=1",
        )
        for order in (records, tuple(reversed(records))):
            with self.subTest(order=order):
                text = "unrelated native output\n\n" + "\n".join(order)
                self.assertTrue(runner.validate_renderer_canaries(text, audit))
        self.assertTrue(runner.validate_renderer_canaries(native_fixture(), audit))

    def test_renderer_canaries_reject_malformed_duplicate_foreign_missing_and_io_errors(
        self,
    ):
        audit = successful_audit()
        first = "CEF_RENDERER_CANARY pid=43211 outcome=denied errno=1\n"
        second = "CEF_RENDERER_CANARY pid=43214 outcome=denied errno=1\n"
        for label, text in {
            "empty": "",
            "unrelated only": "native output\n",
            "missing second renderer": first,
            "missing first renderer": second,
            "duplicate": first + second + first,
            "foreign pid": first + second.replace("43214", "99999"),
            "gpu is not renderer": first + second.replace("43214", "43212"),
            "utility is not renderer": first + second.replace("43214", "43213"),
            "extra foreign record": first + second + first.replace("43211", "99999"),
            "allowed read": first
            + second.replace("outcome=denied errno=1", "outcome=allowed errno=0"),
            "io error": first + second.replace("denied", "error"),
            "zero errno": first + second.replace("errno=1", "errno=0"),
            "missing file is not denial": first + second.replace("errno=1", "errno=2"),
            "other permission error": first + second.replace("errno=1", "errno=13"),
            "negative errno": first + second.replace("errno=1", "errno=-1"),
            "nonnumeric errno": first + second.replace("errno=1", "errno=EPERM"),
            "missing errno": first + second.replace(" errno=1", ""),
            "zero pid": first + second.replace("43214", "0"),
            "negative pid": first + second.replace("43214", "-1"),
            "leading zero pid": first + second.replace("43214", "043214"),
            "oversize pid": first + second.replace("43214", "12345678901"),
            "nonnumeric pid": first + second.replace("43214", "renderer"),
            "extra field": first + second.rstrip() + " extra=1\n",
            "trailing whitespace": first + second.rstrip() + " \n",
            "malformed prefix": first + second + "CEF_RENDERER_CANARY_BROKEN\n",
            "prefixed marker": first + "prefix " + second,
        }.items():
            with self.subTest(case=label):
                self.assertFalse(runner.validate_renderer_canaries(text, audit))
        without_renderers = {
            **audit,
            "helpers": [h for h in audit["helpers"] if h["role"] != "renderer"],
        }
        for text in ("", first + second):
            with self.subTest(no_renderers=text):
                self.assertFalse(
                    runner.validate_renderer_canaries(text, without_renderers)
                )

    def test_profile_rejects_permissions_marker_symlinks_and_nonfresh_content(self):
        profile = self.profile()
        profile.path.chmod(0o755)
        with self.assertRaises(runner.ProbeError):
            runner.validate_profile(profile.path)
        profile.path.chmod(0o700)
        marker = profile.path / runner.MARKER_NAME
        marker.write_bytes(b"wrong")
        with self.assertRaises(runner.ProbeError):
            runner.validate_profile(profile.path)
        marker.unlink()
        marker.symlink_to(self.fixture)
        with self.assertRaises(runner.ProbeError):
            runner.validate_profile(profile.path)
        marker.unlink()
        marker.write_bytes(runner.MARKER)
        (profile.path / "cef-cache").mkdir()
        with self.assertRaisesRegex(runner.ProbeError, "fresh"):
            runner.validate_profile(profile.path, fresh=True)
        runner.validate_profile(profile.path)
        with self.assertRaises(runner.ProbeError):
            runner.validate_profile(self.base)
        alias = self.base / "home-link"
        alias.symlink_to(profile.path)
        with self.assertRaises(runner.ProbeError):
            runner.validate_profile(alias)

    def test_profile_environment_rejects_nonowned_origins_or_nonroot_paths(self):
        profile = self.profile()
        for url in (
            "https://127.0.0.1:3/",
            "http://localhost:3/",
            "http://127.0.0.2:3/",
            "http://u:p@127.0.0.1:3/",
            "http://127.0.0.1/",
            "http://127.0.0.1:3/x",
            "http://127.0.0.1:3/cef",
            "http://127.0.0.1:3/webkit",
            "http://127.0.0.1:3/?x",
            "http://127.0.0.1:3/#x",
        ):
            with (
                self.subTest(url=url),
                self.assertRaises((runner.ProbeError, ValueError)),
            ):
                runner.launch_environment({}, profile, url)

    def test_profile_is_never_deleted_while_helper_exit_is_unverified(self):
        for success, root, reason in (
            (False, Root(0), "unsuccessful run/shutdown"),
            (True, Root(), "owned root stop is unconfirmed"),
            (
                True,
                Mock(poll=Mock(side_effect=OSError("unconfirmed"))),
                "owned root stop is unconfirmed",
            ),
        ):
            profile = self.profile()
            with redirect_stderr(io.StringIO()) as diagnostics:
                self.assertEqual(
                    runner.finish_profile(profile, success, [root]), "preserved"
                )
            self.assertTrue(profile.path.exists())
            self.assertIn(str(profile.path), diagnostics.getvalue())
            self.assertIn(reason, diagnostics.getvalue())
        profile = self.profile()
        (profile.path / runner.MARKER_NAME).write_bytes(b"wrong")
        with redirect_stderr(io.StringIO()):
            self.assertEqual(
                runner.finish_profile(profile, True, [Root(0)]),
                "retained-helper-exit-unverified",
            )
        self.assertEqual((profile.path / runner.MARKER_NAME).read_bytes(), b"wrong")
        profile = self.profile()
        helper_cache = profile.path / "helper-cache"
        helper_cache.write_bytes(b"a helper may still be using this")
        with (
            redirect_stderr(io.StringIO()) as diagnostics,
            patch("shutil.rmtree") as delete,
        ):
            self.assertEqual(
                runner.finish_profile(profile, True, [Root(0)]),
                "retained-helper-exit-unverified",
            )
        delete.assert_not_called()
        self.assertIn("native root passed", diagnostics.getvalue())
        self.assertTrue(profile.path.exists())
        self.assertEqual(helper_cache.read_bytes(), b"a helper may still be using this")

    def test_verified_helper_exit_retains_artifacts_only_after_success_and_root_stop(
        self,
    ):
        for success, code, expected in (
            (True, 0, "retained-audit-artifacts"),
            (False, 0, "preserved"),
            (True, None, "preserved"),
        ):
            with self.subTest(success=success, code=code):
                profile = self.profile()
                artifact = profile.path / "audit-artifact"
                artifact.write_bytes(b"retain for inspection")
                with (
                    redirect_stderr(io.StringIO()) as diagnostics,
                    patch("shutil.rmtree") as delete,
                ):
                    self.assertEqual(
                        runner.finish_profile(
                            profile, success, [Root(code)], helper_exit_verified=True
                        ),
                        expected,
                    )
                delete.assert_not_called()
                self.assertTrue(profile.path.exists())
                self.assertEqual(artifact.read_bytes(), b"retain for inspection")
                if expected == "retained-audit-artifacts":
                    self.assertIn(
                        "announced helper exits verified", diagnostics.getvalue()
                    )
                    self.assertNotIn("unverified", diagnostics.getvalue())

    def test_output_is_fresh_private_and_private_logs_refuse_overwrite(self):
        with patch.object(runner, "TARGET", self.base):
            first, second = runner.create_output(), runner.create_output()
        self.assertNotEqual(first, second)
        self.assertTrue(first.name.startswith("cef-probe-run-"))
        self.assertEqual(first.stat().st_mode & 0o777, 0o700)
        log = first / "private.log"
        with runner.private_log(log) as output:
            output.write(b"owned")
        self.assertEqual(log.stat().st_mode & 0o777, 0o600)
        with self.assertRaises(FileExistsError):
            runner.private_log(log)
        self.assertEqual(log.read_bytes(), b"owned")

    def test_local_fixture_http_routes_headers_host_and_shutdown_without_logging(self):
        with redirect_stderr(io.StringIO()) as diagnostics:
            with runner.fixture_server() as url:
                parsed = urlsplit(url)
                self.assertEqual(parsed.hostname, "127.0.0.1")
                for method, path, status in (
                    ("GET", "/", 200),
                    ("GET", "/favicon.ico", 204),
                    ("GET", "/unknown", 404),
                    ("GET", "/../secret", 404),
                    ("GET", "/?query", 404),
                    ("POST", "/", 405),
                    ("HEAD", "/", 405),
                ):
                    connection = http.client.HTTPConnection(
                        parsed.hostname, parsed.port, timeout=2
                    )
                    connection.request(method, path)
                    response = connection.getresponse()
                    self.assertEqual(response.status, status)
                    self.assertEqual(
                        response.getheader("Content-Security-Policy"), runner.CSP
                    )
                    self.assertEqual(response.getheader("Cache-Control"), "no-store")
                    body = response.read()
                    self.assertEqual(
                        body, self.fixture.read_bytes() if status == 200 else b""
                    )
                    connection.close()
                connection = http.client.HTTPConnection(
                    parsed.hostname, parsed.port, timeout=2
                )
                connection.request("GET", "/", headers={"Host": "attacker.invalid"})
                self.assertEqual(connection.getresponse().status, 403)
                connection.close()
            connection = http.client.HTTPConnection(
                parsed.hostname, parsed.port, timeout=0.2
            )
            try:
                with self.assertRaises(OSError):
                    connection.request("GET", "/")
            finally:
                connection.close()
        self.assertEqual(diagnostics.getvalue(), "")

    def test_coexistence_fixture_serves_only_exact_engine_routes_with_same_policy(self):
        with redirect_stderr(io.StringIO()) as diagnostics:
            with runner.fixture_server(True) as url:
                parsed = urlsplit(url)
                self.assertEqual(url, f"http://127.0.0.1:{parsed.port}/")
                cases = [
                    ("GET", "/cef", None, 200),
                    ("GET", "/webkit", None, 200),
                    ("GET", "/", None, 404),
                    ("GET", "/favicon.ico", None, 204),
                ]
                for route in ("/cef", "/webkit"):
                    cases.extend(
                        ("GET", path, f"127.0.0.1:{parsed.port}", 404)
                        for path in (
                            route + "?query",
                            route + "/",
                            route + "/../secret",
                            "/%63ef",
                            "/unknown",
                            url + route.lstrip("/"),
                            "http://attacker.invalid" + route,
                        )
                    )
                    cases.append(("GET", route, "attacker.invalid", 403))
                    cases.append(("GET", route, f"localhost:{parsed.port}", 403))
                    cases.extend(
                        (method, route, None, 405)
                        for method in (
                            "POST",
                            "HEAD",
                            "PUT",
                            "DELETE",
                            "OPTIONS",
                            "PATCH",
                            "TRACE",
                            "CONNECT",
                        )
                    )
                for method, path, host, status in cases:
                    with self.subTest(method=method, path=path, host=host):
                        connection = http.client.HTTPConnection(
                            parsed.hostname, parsed.port, timeout=2
                        )
                        try:
                            connection.request(
                                method, path, headers={"Host": host} if host else {}
                            )
                            response = connection.getresponse()
                            self.assertEqual(response.status, status)
                            self.assertEqual(
                                response.getheader("Content-Security-Policy"),
                                runner.CSP,
                            )
                            self.assertEqual(
                                response.getheader("Cache-Control"), "no-store"
                            )
                            self.assertEqual(
                                response.getheader("X-Content-Type-Options"), "nosniff"
                            )
                            self.assertEqual(
                                response.getheader("Referrer-Policy"), "no-referrer"
                            )
                            self.assertEqual(response.getheader("Connection"), "close")
                            body = response.read()
                            self.assertEqual(
                                body,
                                self.coex_fixture.read_bytes()
                                if status == 200
                                else b"",
                            )
                            if status == 200:
                                self.assertEqual(
                                    response.getheader("Content-Type"),
                                    "text/html; charset=utf-8",
                                )
                        finally:
                            connection.close()
            connection = http.client.HTTPConnection(
                parsed.hostname, parsed.port, timeout=0.2
            )
            try:
                with self.assertRaises(OSError):
                    connection.request("GET", "/cef")
            finally:
                connection.close()
        self.assertEqual(diagnostics.getvalue(), "")

    def test_fixture_modes_do_not_expose_each_others_routes_or_files(self):
        with runner.fixture_server() as url:
            parsed = urlsplit(url)
            for route in ("/cef", "/webkit", "/coexistence.html"):
                with self.subTest(route=route):
                    connection = http.client.HTTPConnection(
                        parsed.hostname, parsed.port, timeout=2
                    )
                    try:
                        connection.request("GET", route)
                        response = connection.getresponse()
                        self.assertEqual(response.status, 404)
                        self.assertEqual(response.read(), b"")
                    finally:
                        connection.close()
        self.fixture.unlink()
        with runner.fixture_server(True):
            pass
        self.coex_fixture.unlink()
        with self.assertRaises((ValueError, OSError)):
            with runner.fixture_server(True):
                self.fail("missing coexistence fixture must not fall back")

    def test_coexistence_fixture_rejects_oversized_and_redirected_files_before_serving(
        self,
    ):
        self.coex_fixture.write_bytes(b"x" * (1024**2 + 1))
        with self.assertRaisesRegex(runner.ProbeError, "Oversized"):
            with runner.fixture_server(True):
                self.fail("oversized fixture served")
        self.coex_fixture.unlink()
        self.coex_fixture.symlink_to(self.fixture)
        with self.assertRaises((ValueError, OSError)):
            with runner.fixture_server(True):
                self.fail("redirected fixture served")

    def test_server_closes_even_if_body_raises(self):
        with self.assertRaisesRegex(runner.ProbeError, "interrupted"):
            with runner.fixture_server() as url:
                parsed = urlsplit(url)
                raise runner.ProbeError("interrupted")
        connection = http.client.HTTPConnection(
            parsed.hostname, parsed.port, timeout=0.2
        )
        try:
            with self.assertRaises(OSError):
                connection.request("GET", "/")
        finally:
            connection.close()

    def test_completion_requires_exact_ordered_unique_markers_and_zero_exit(self):
        complete = "\n".join(runner.MARKERS) + "\n"
        runner.validate_completion(complete, 0)
        for code in (None, 1, -9):
            with self.subTest(code=code), self.assertRaises(runner.ProbeError):
                runner.validate_completion(complete, code)
        for index in range(len(runner.MARKERS)):
            with self.subTest(missing=index), self.assertRaises(runner.ProbeError):
                runner.validate_completion(
                    "\n".join(
                        marker for i, marker in enumerate(runner.MARKERS) if i != index
                    ),
                    0,
                )
        for text in (
            complete + "FAIL: fixture failure\n",
            complete + runner.MARKERS[0],
            "\n".join(reversed(runner.MARKERS)),
            complete.replace(runner.MARKERS[0], "prefix " + runner.MARKERS[0]),
        ):
            with self.subTest(text=text), self.assertRaises(runner.ProbeError):
                runner.validate_completion(text, 0)

    def test_coexistence_markers_match_the_native_contract(self):
        self.assertEqual(
            runner.COEX_MARKERS,
            (
                "PASS: system WKWebView and CEF loaded in one GPUI window",
                "PASS: WebKit and CEF storage contexts stayed isolated",
                "PASS: native focus and input moved WebKit to CEF to WebKit",
                "PASS: both engine viewports followed the GPUI resize",
                "PASS: WebKit detached before CEF shutdown",
                "PASS: CEF OnBeforeClose, callback release, pump cancellation and shutdown before GPUI termination",
                "PASS: original GPUI shutdown callback after CEF shutdown",
            ),
        )

    def test_coexistence_completion_requires_every_exact_ordered_unique_marker(self):
        markers = runner.COEX_MARKERS
        complete = "\n".join(markers) + "\n"
        runner.validate_completion(complete, 0, markers)
        runner.validate_completion(native_fixture(markers), 0, markers=markers)
        for code in (None, 1, -9):
            with self.subTest(code=code), self.assertRaises(runner.ProbeError):
                runner.validate_completion(complete, code, markers)
        invalid = [
            "",
            complete + "FAIL: fixture failure\n",
            "\n".join(reversed(markers)),
        ]
        for index, marker in enumerate(markers):
            invalid.extend(
                (
                    "\n".join(markers[:index] + markers[index + 1 :]),
                    complete + marker + "\n",
                    complete.replace(marker, "prefix " + marker),
                    complete.replace(marker, marker + " suffix"),
                )
            )
            if index + 1 < len(markers):
                swapped = list(markers)
                swapped[index : index + 2] = reversed(swapped[index : index + 2])
                invalid.append("\n".join(swapped))
        for index, text in enumerate(invalid):
            with self.subTest(invalid=index), self.assertRaises(runner.ProbeError):
                runner.validate_completion(text, 0, markers)

    def test_completion_refuses_wrong_mode_and_mixed_mode_markers(self):
        for selected, other in (
            (runner.MARKERS, runner.COEX_MARKERS),
            (runner.COEX_MARKERS, runner.MARKERS),
        ):
            with self.subTest(selected=selected):
                with self.assertRaises(runner.ProbeError):
                    runner.validate_completion("\n".join(other), 0, selected)
                for foreign in set(other) - set(selected):
                    with (
                        self.subTest(foreign=foreign),
                        self.assertRaisesRegex(runner.ProbeError, "another probe mode"),
                    ):
                        runner.validate_completion(
                            "\n".join((*selected, foreign)), 0, selected
                        )

    def test_log_inspection_and_native_budget_are_bounded(self):
        log = self.output / "native.log"
        log.write_bytes(b"x" * 20)
        with (
            patch.object(runner, "MAX_LOG_BYTES", 10),
            self.assertRaises(runner.ProbeError),
        ):
            runner.read_log(log)
        with patch.object(runner.time, "monotonic", return_value=100):
            budget = runner.Budget()
        with patch.object(runner.time, "monotonic", return_value=159):
            self.assertEqual(budget.remaining(5), 1)
        with (
            patch.object(runner.time, "monotonic", return_value=160),
            self.assertRaises(runner.ProbeError),
        ):
            budget.remaining()

    def test_builds_prevalidate_each_sdk_and_use_only_exact_commands_clean_environment(
        self,
    ):
        calls = []

        def command(argv, env, path, roots, timeout):
            calls.append((argv, env, path, timeout))

        with (
            patch.object(
                runner.bundle_probe,
                "verify_prepared_sdk",
                return_value=(runner.SDK, {}),
            ) as verify,
            patch.object(runner, "run_command", side_effect=command),
        ):
            runner.build_binaries(
                {
                    "CEF_PATH": "/wrong",
                    "FLATPAK": "1",
                    "DYLD_INSERT_LIBRARIES": "/bad",
                    "PATH": "/usr/bin",
                },
                self.output,
                [],
            )
        self.assertEqual(verify.call_count, 2)
        self.assertEqual([call[0] for call in calls], runner.build_commands())
        for command, env, _, timeout in calls:
            self.assertEqual(timeout, 180)
            self.assertEqual(env, {"PATH": "/usr/bin", "CEF_PATH": str(runner.SDK)})
            self.assertIn("--locked", command)
            self.assertEqual(
                command[command.index("--target-dir") + 1], str(runner.TARGET)
            )
        with (
            patch.object(
                runner.bundle_probe,
                "verify_prepared_sdk",
                side_effect=ValueError("pin mismatch"),
            ),
            patch.object(runner, "run_command") as command,
        ):
            with self.assertRaises(ValueError):
                runner.build_binaries({}, self.output, [])
            command.assert_not_called()

    def test_coexistence_build_selects_only_the_host_feature_and_still_prevalidates_sdk(
        self,
    ):
        default = runner.build_commands()
        coex = runner.build_commands(True)
        self.assertEqual(coex[0][:-1], default[0][:-1])
        self.assertEqual(default[0][-2:], ["--features", "runtime"])
        self.assertEqual(coex[0][-2:], ["--features", "coexistence"])
        self.assertEqual(coex[1], default[1])
        self.assertNotIn("--features", coex[1])
        with (
            patch.object(
                runner.bundle_probe,
                "verify_prepared_sdk",
                return_value=(runner.SDK, {}),
            ) as verify,
            patch.object(runner, "run_command") as command,
        ):
            runner.build_binaries(
                {"CEF_PATH": "/wrong", "FELINAUT_ENGINE": "cef"}, self.output, [], True
            )
        self.assertEqual(verify.call_count, 2)
        self.assertEqual([call.args[0] for call in command.call_args_list], coex)
        for index, call in enumerate(command.call_args_list):
            self.assertEqual(call.args[1], {"CEF_PATH": str(runner.SDK)})
            self.assertEqual(call.args[2], self.output / f"build-{index + 1}.log")
            self.assertEqual(call.args[4], 180)
        with (
            patch.object(
                runner.bundle_probe,
                "verify_prepared_sdk",
                side_effect=ValueError("pin mismatch"),
            ),
            patch.object(runner, "run_command") as command,
            self.assertRaisesRegex(ValueError, "pin mismatch"),
        ):
            runner.build_binaries({}, self.output, [], True)
        command.assert_not_called()

    def test_spawn_and_timeout_track_only_owned_root_without_process_groups(self):
        root, roots = Root(), []
        log = self.output / "command.log"
        with patch.object(runner.subprocess, "Popen", return_value=root) as popen:
            with self.assertRaisesRegex(runner.ProbeError, "timed out"):
                runner.run_command(
                    ["owned-command", "arg"], {"safe": "env"}, log, roots, 180
                )
        self.assertEqual(roots, [root])
        self.assertEqual(popen.call_args.args[0], ["owned-command", "arg"])
        self.assertEqual(popen.call_args.kwargs["stdin"], subprocess.DEVNULL)
        self.assertEqual(popen.call_args.kwargs["stderr"], subprocess.STDOUT)
        self.assertNotIn("start_new_session", popen.call_args.kwargs)
        self.assertEqual(log.stat().st_mode & 0o777, 0o600)
        runner.stop_owned(roots)
        self.assertTrue(root.terminated)
        self.assertIsNotNone(root.poll())

    def test_cleanup_escalates_only_owned_process_and_reports_unconfirmed(self):
        root = Mock()
        root.poll.return_value = None
        root.wait.side_effect = [subprocess.TimeoutExpired("owned", 2), 0]
        with patch.object(os, "killpg", side_effect=AssertionError("global kill")):
            runner.stop_owned([root])
        root.terminate.assert_called_once()
        root.kill.assert_called_once()
        self.assertEqual(
            [call.kwargs["timeout"] for call in root.wait.call_args_list], [2, 1]
        )
        root.wait.side_effect = subprocess.TimeoutExpired("owned", 1)
        with self.assertRaisesRegex(runner.ProbeError, "Could not stop"):
            runner.stop_owned([root])

    def test_capture_helper_compile_contract_and_timeout(self):
        with patch.object(runner, "run_command") as command:
            helper = runner.compile_capture_helper({}, self.output, [])
        argv, env, log, roots, timeout = command.call_args.args
        self.assertEqual(argv[:2], ["/usr/bin/xcrun", "swiftc"])
        self.assertIn(runner.ROOT / "scripts/backend_window.swift", argv)
        self.assertIn("-module-cache-path", argv)
        self.assertEqual(helper, self.output / "backend-window")
        self.assertEqual(timeout, 60)

    def test_capture_only_owned_window_and_sanitizes_before_publication(self):
        root, commands = Root(), []

        def command(argv, env, log, roots, timeout):
            commands.append(argv)
            with runner.private_log(log) as output:
                if argv[0] != "/usr/sbin/screencapture":
                    output.write(b"321\n")
                else:
                    Path(argv[-1]).write_bytes(png_fixture())

        with patch.object(runner, "run_command", side_effect=command):
            capture = runner.capture_root(
                root, self.output / "selector", self.output, {}, [], runner.Budget()
            )
        self.assertEqual(commands[0], [self.output / "selector", str(root.pid)])
        self.assertEqual(
            commands[1][:5], ["/usr/sbin/screencapture", "-x", "-o", "-l", "321"]
        )
        self.assertTrue(capture.exists())
        self.assertNotIn(b"private fixture metadata", capture.read_bytes())
        self.assertFalse(list(self.output.glob(".capture-*")))

    def test_capture_refuses_invalid_window_or_stopped_root_without_display_fallback(
        self,
    ):
        root = Root()

        def invalid(argv, env, log, roots, timeout):
            log.write_text("not an owned window")

        with patch.object(runner, "run_command", side_effect=invalid) as command:
            with self.assertRaises(runner.ProbeError):
                runner.capture_root(
                    root, self.output / "selector", self.output, {}, [], runner.Budget()
                )
        self.assertEqual(command.call_count, 1)
        with patch.object(runner, "run_command") as command:
            with self.assertRaises(runner.ProbeError):
                runner.capture_root(
                    Root(0),
                    self.output / "selector",
                    self.output,
                    {},
                    [],
                    runner.Budget(),
                )
        command.assert_not_called()
        self.assertFalse((self.output / "cef-probe-window.png").exists())

    def test_failed_capture_never_publishes_raw_png(self):
        root = Root()

        def fail(argv, env, log, roots, timeout):
            if argv[0] != "/usr/sbin/screencapture":
                log.write_text("321")
            else:
                Path(argv[-1]).write_bytes(png_fixture())
                raise runner.ProbeError("capture denied")

        with patch.object(runner, "run_command", side_effect=fail):
            with self.assertRaisesRegex(runner.ProbeError, "capture denied"):
                runner.capture_root(
                    root, self.output / "selector", self.output, {}, [], runner.Budget()
                )
        self.assertFalse((self.output / "cef-probe-window.png").exists())
        self.assertFalse(list(self.output.glob(".capture-*")))

    def test_monitor_captures_at_viewport_marker_then_requires_shutdown(self):
        root = Root()
        log = self.output / "native.log"
        log.write_text("\n".join(runner.MARKERS[:3]) + "\n")
        image = self.output / "capture.png"

        def capture(*args):
            with log.open("a") as output:
                output.write("\n".join(runner.MARKERS[3:]) + "\n")
            root.returncode = 0
            return image

        with patch.object(runner, "capture_root", side_effect=capture) as capture:
            self.assertEqual(
                runner.monitor(
                    root, log, self.output, {}, [], self.output / "selector"
                ),
                image,
            )
        capture.assert_called_once()
        log.write_text("\n".join(runner.MARKERS[:2]))
        with (
            patch.object(runner, "capture_root") as capture,
            self.assertRaises(runner.ProbeError),
        ):
            runner.monitor(root, log, self.output, {}, [], self.output / "selector")
        capture.assert_not_called()

    def test_coexistence_monitor_waits_for_both_viewports_before_capture_and_shutdown(
        self,
    ):
        root = Root()
        log = self.output / "native.log"
        log.write_text("\n".join(runner.COEX_MARKERS[:3]) + "\n")
        image = self.output / "capture.png"
        sleeps = []

        def resize(_):
            sleeps.append(True)
            with log.open("a") as output:
                output.write(runner.COEX_MARKERS[3] + "\n")

        def capture(*args):
            self.assertEqual(sleeps, [True], "capture must wait for the resize marker")
            with log.open("a") as output:
                output.write("\n".join(runner.COEX_MARKERS[4:]) + "\n")
            root.returncode = 0
            return image

        observer = Mock(spec=["observe"])
        with (
            patch.object(runner.time, "sleep", side_effect=resize),
            patch.object(runner, "capture_root", side_effect=capture) as captured,
        ):
            self.assertEqual(
                runner.monitor(
                    root,
                    log,
                    self.output,
                    {},
                    [],
                    self.output / "selector",
                    observer=observer,
                    markers=runner.COEX_MARKERS,
                    viewport_marker=runner.COEX_MARKERS[3],
                ),
                image,
            )
        captured.assert_called_once()
        self.assertEqual(
            "".join(call.args[0] for call in observer.observe.call_args_list),
            "\n".join(runner.COEX_MARKERS) + "\n",
        )
        log.write_text("\n".join(runner.COEX_MARKERS[:4]) + "\n")
        with (
            patch.object(runner, "capture_root", return_value=image),
            self.assertRaisesRegex(runner.ProbeError, "missing or duplicates"),
        ):
            runner.monitor(
                root,
                log,
                self.output,
                {},
                [],
                self.output / "selector",
                markers=runner.COEX_MARKERS,
                viewport_marker=runner.COEX_MARKERS[3],
            )
        log.write_text(native_fixture())
        with (
            patch.object(runner, "capture_root") as captured,
            self.assertRaises(runner.ProbeError),
        ):
            runner.monitor(
                root,
                log,
                self.output,
                {},
                [],
                self.output / "selector",
                markers=runner.COEX_MARKERS,
                viewport_marker=runner.COEX_MARKERS[3],
            )
        captured.assert_not_called()

    def test_monitor_feeds_only_complete_appended_lines_once_including_exit_tail(self):
        root = Root()
        root.poll = Mock(side_effect=[None, None, None, 0])
        observer = Mock(spec=["observe"])
        first = "CEF_HELPER_LAUNCH\n"
        second = (
            "CEF_HELPER_START pid=43211 ppid=43210 role=renderer "
            "subtype=none sandbox=renderer seatbelt=1\n"
        )
        complete = "\n".join(runner.MARKERS) + "\n"
        snapshots = [
            first[:8],
            first + second[:20],
            first + second[:20],
            first + second + complete[:10],
            first + second + complete + "CEF_HELPER_SANDBOX_READY pid=",
        ]
        with (
            patch.object(runner, "read_log", side_effect=snapshots) as read,
            patch.object(runner.time, "sleep"),
        ):
            self.assertIsNone(
                runner.monitor(
                    root,
                    self.output / "native.log",
                    self.output,
                    {},
                    [],
                    budget=Mock(remaining=Mock(return_value=1)),
                    observer=observer,
                )
            )
        self.assertEqual(read.call_count, len(snapshots))
        self.assertEqual(
            [call.args[0] for call in observer.observe.call_args_list],
            [first, second, complete],
        )
        self.assertEqual(root.poll.call_count, 4)

    def test_monitor_does_not_redeliver_unchanged_final_log_to_observer(self):
        log = self.output / "native.log"
        log.write_text(native_fixture())
        observer = Mock(spec=["observe"])
        self.assertIsNone(
            runner.monitor(Root(0), log, self.output, {}, [], observer=observer)
        )
        observer.observe.assert_called_once_with(native_fixture())

    @contextmanager
    def execution(self, code=0, text=None, audit=None):
        root = Root(code)
        audit = successful_audit() if audit is None else audit
        observer = Mock(spec=["observe", "finish"])
        state = {
            "server_stopped": False,
            "root": root,
            "observer": observer,
            "audit": audit,
        }
        original_profile = runner.create_profile

        def observe(root_pid, app, canary, allowed):
            self.assertIn("command", state, "observer must be created after spawn")
            self.assertEqual(root_pid, root.pid)
            self.assertEqual(app, self.output / "probe.app")
            self.assertEqual(canary, self.output / "sandbox-canary")
            self.assertEqual(
                allowed,
                app
                / "Contents/Frameworks/Chromium Embedded Framework.framework"
                / "Chromium Embedded Framework",
            )
            self.assertEqual(canary.read_bytes(), runner.CANARY_BYTES)
            return observer

        def finish(*, deadline_seconds):
            state["exit_at_finish"] = root.poll()
            return audit

        observer.finish.side_effect = finish

        def profile():
            value = original_profile()
            self.profiles.append(value)
            state["profile"] = value
            return value

        @contextmanager
        def server(coexistence=False):
            state["server_coexistence"] = coexistence
            try:
                yield "http://127.0.0.1:45678/"
            finally:
                state["server_stopped"] = True

        def spawn(argv, env, log, roots):
            state["command"], state["env"] = argv, env
            log.write((text if text is not None else native_fixture()).encode())
            roots.append(root)
            return root

        with (
            patch.object(runner.platform, "system", return_value="Darwin"),
            patch.object(runner.platform, "machine", return_value="arm64"),
            patch.object(
                runner.bundle_probe,
                "verify_prepared_sdk",
                return_value=(runner.SDK, {}),
            ),
            patch.object(runner, "create_output", return_value=self.output),
            patch.object(
                runner.bundle_probe,
                "create_bundle",
                return_value=self.output / "probe.app",
            ),
            patch.object(runner, "create_profile", side_effect=profile),
            patch.object(runner, "fixture_server", side_effect=server),
            patch.object(runner, "spawn", side_effect=spawn),
            patch.object(
                runner.helper_observer, "HelperObserver", side_effect=observe
            ) as create_observer,
            redirect_stdout(io.StringIO()),
            redirect_stderr(io.StringIO()),
        ):
            state["create_observer"] = create_observer
            yield state

    def test_successful_mock_native_run_reports_success_and_retains_home(self):
        with self.execution() as state:
            report = runner.execute()
        self.assertTrue(report["success"])
        self.assertEqual(report["mode"], "cef")
        self.assertFalse(state["server_coexistence"])
        self.assertTrue(state["server_stopped"])
        self.assertTrue(state["profile"].path.exists())
        self.assertEqual(report["home_cleanup"], "retained-audit-artifacts")
        self.assertTrue(report["home_retained"])
        self.assertEqual(json.loads((self.output / "report.json").read_text()), report)
        self.assertEqual(report["root_exit"], 0)
        self.assertEqual(
            state["command"],
            [self.output / "probe.app/Contents/MacOS/felinaut-cef-probe", "--run-cef"],
        )
        self.assertEqual(state["env"]["HOME"], str(state["profile"].path))
        self.assertEqual(report["helper_lifecycle"], "verified-announced-helper-exit")
        self.assertEqual(report["helper_audit"], state["audit"])
        self.assertTrue(report["notification_broker_absent"])
        self.assertTrue(report["helper_audit_passed"])
        self.assertTrue(report["renderer_canary_denied"])
        self.assertTrue(report["canary_control_unchanged"])
        state["create_observer"].assert_called_once()
        state["observer"].observe.assert_called_once_with(native_fixture())
        state["observer"].finish.assert_called_once_with(deadline_seconds=3)
        self.assertEqual(state["exit_at_finish"], 0)
        self.assertEqual(
            state["env"]["FELINAUT_CEF_PROBE_CANARY"],
            str(self.output / "sandbox-canary"),
        )
        for name in ("native.log", "report.json", "sandbox-canary"):
            self.assertEqual((self.output / name).stat().st_mode & 0o777, 0o600)

    def test_mock_coexistence_cli_builds_host_feature_launches_and_reports_strict_audit(
        self,
    ):
        text = native_fixture(runner.COEX_MARKERS)
        with (
            self.execution(text=text) as state,
            patch.dict(
                runner.os.environ, {"FELINAUT_ENGINE": "cef", "FELINAUT_AGENT": "1"}
            ),
            patch.object(runner, "run_command") as build,
            patch.object(
                runner, "compile_capture_helper", return_value=self.output / "selector"
            ),
            patch.object(
                runner,
                "capture_root",
                return_value=self.output / "cef-probe-window.png",
            ) as capture,
        ):
            self.assertEqual(
                runner.main(["--run", "--build", "--capture", "--coexistence"]), 0
            )
        report = json.loads((self.output / "report.json").read_text())
        self.assertTrue(report["success"])
        self.assertEqual(report["mode"], "coexistence")
        self.assertEqual(
            [call.args[0] for call in build.call_args_list], runner.build_commands(True)
        )
        self.assertEqual(
            state["command"],
            [
                self.output / "probe.app/Contents/MacOS/felinaut-cef-probe",
                "--run-coexistence",
            ],
        )
        self.assertTrue(state["server_coexistence"])
        self.assertTrue(state["server_stopped"])
        self.assertEqual(report["fixture_url"], "http://127.0.0.1:45678/")
        self.assertEqual(state["env"]["FELINAUT_CEF_PROBE_URL"], report["fixture_url"])
        self.assertEqual(state["env"]["HOME"], str(state["profile"].path))
        self.assertEqual(state["env"]["CFFIXED_USER_HOME"], str(state["profile"].path))
        self.assertEqual(
            state["env"]["FELINAUT_CEF_PROBE_HOME"], str(state["profile"].path)
        )
        self.assertEqual(
            (state["profile"].path / runner.MARKER_NAME).read_bytes(), runner.MARKER
        )
        self.assertNotIn("FELINAUT_ENGINE", state["env"])
        self.assertNotIn("FELINAUT_AGENT", state["env"])
        self.assertEqual(report["helper_audit"], state["audit"])
        self.assertEqual(report["helper_lifecycle"], "verified-announced-helper-exit")
        for key in (
            "helper_audit_passed",
            "renderer_canary_denied",
            "notification_broker_absent",
            "canary_control_unchanged",
            "home_retained",
        ):
            self.assertTrue(report[key], key)
        self.assertEqual(report["home_cleanup"], "retained-audit-artifacts")
        self.assertEqual(report["root_exit"], 0)
        self.assertTrue(state["profile"].path.exists())
        state["create_observer"].assert_called_once()
        state["observer"].observe.assert_called_once_with(text)
        state["observer"].finish.assert_called_once_with(deadline_seconds=3)
        self.assertEqual(state["exit_at_finish"], 0)
        capture.assert_called_once()
        self.assertEqual(report["capture"], str(self.output / "cef-probe-window.png"))

    def test_mock_coexistence_refuses_cef_only_mixed_missing_and_nonzero_completion(
        self,
    ):
        complete = native_fixture(runner.COEX_MARKERS)
        for index, (text, code) in enumerate(
            (
                (native_fixture(), 0),
                (complete + runner.MARKERS[0] + "\n", 0),
                (complete.replace(runner.COEX_MARKERS[4] + "\n", ""), 0),
                (complete, 1),
            )
        ):
            with self.subTest(case=index):
                self.output = self.base / f"coex-completion-{index}"
                self.output.mkdir(mode=0o700)
                with self.execution(text=text, code=code) as state:
                    with self.assertRaises(runner.ProbeError):
                        runner.execute(coexistence=True)
                report = json.loads((self.output / "report.json").read_text())
                self.assertEqual(report["mode"], "coexistence")
                self.assertFalse(report["success"])
                self.assertTrue(report["helper_audit_passed"])
                self.assertEqual(report["home_cleanup"], "preserved")
                self.assertTrue(state["profile"].path.exists())
                self.assertTrue(state["server_stopped"])
                state["observer"].finish.assert_called_once_with(deadline_seconds=3)

    def test_mock_coexistence_keeps_all_audit_gates_mandatory(self):
        for case in (
            "audit-gap",
            "no-helpers",
            "missing-renderer",
            "missing-gpu-process",
            "missing-utility",
            "seatbelt-renderer",
            "seatbelt-gpu-process",
            "seatbelt-utility",
            "notification-broker",
            "renderer-canary",
            "changed-canary",
        ):
            with self.subTest(case=case):
                self.output = self.base / ("coex-" + case)
                self.output.mkdir(mode=0o700)
                audit = successful_audit()
                text = native_fixture(runner.COEX_MARKERS)
                if case == "audit-gap":
                    audit["ok"] = False
                    audit["gaps"] = ["announced helper exit is unverified"]
                elif case == "no-helpers":
                    audit["helpers"] = []
                    for role in audit["roles"].values():
                        role["count"] = 0
                elif case.startswith("missing-"):
                    audit["roles"][case.removeprefix("missing-")]["count"] = 0
                elif case.startswith("seatbelt-"):
                    role = case.removeprefix("seatbelt-")
                    next(
                        helper for helper in audit["helpers"] if helper["role"] == role
                    )["seatbelt"] = 0
                elif case == "notification-broker":
                    audit["helpers"][2]["subtype"] = (
                        "mac_notifications.mojom.MacNotificationProvider"
                    )
                elif case == "renderer-canary":
                    text = text.replace(
                        "CEF_RENDERER_CANARY pid=43214 outcome=denied errno=1\n", ""
                    )
                with (
                    self.execution(text=text, audit=audit) as state,
                    patch.object(
                        runner, "validate_canary", wraps=runner.validate_canary
                    ) as control,
                ):
                    if case == "changed-canary":
                        control.side_effect = [(1, 2, 3, 4), (1, 2, 3, 5)]
                    with self.assertRaisesRegex(
                        runner.ProbeError, "inspect report.json"
                    ):
                        runner.execute(False, False, True)
                report = json.loads((self.output / "report.json").read_text())
                self.assertEqual(report["mode"], "coexistence")
                self.assertFalse(report["success"])
                self.assertFalse(report["helper_audit_passed"])
                self.assertEqual(report["helper_audit"], audit)
                self.assertEqual(report["root_exit"], 0)
                self.assertEqual(report["home_cleanup"], "preserved")
                self.assertTrue(report["home_retained"])
                self.assertTrue(state["profile"].path.exists())
                self.assertTrue(state["server_stopped"])
                state["observer"].finish.assert_called_once_with(deadline_seconds=3)

    def test_successful_cli_exit_does_not_treat_retained_home_as_failure(self):
        with self.execution() as state:
            self.assertEqual(runner.main(["--run"]), 0)
        report = json.loads((self.output / "report.json").read_text())
        self.assertTrue(report["success"])
        self.assertTrue(report["home_retained"])
        self.assertEqual(report["home_cleanup"], "retained-audit-artifacts")
        self.assertTrue(state["profile"].path.exists())

    def test_execute_separates_build_native_and_capture_environments(self):
        inherited = {
            "HOME": "/desktop/home",
            "TMPDIR": "/desktop/tmp/",
            "PATH": "/build/bin",
            "CARGO_HOME": "/cargo",
            "DEVELOPER_DIR": "/Xcode",
            "LANG": "C",
            "GITHUB_TOKEN": "sentinel",
            "SSLKEYLOGFILE": "/outside/keys",
            "DYLD_INSERT_LIBRARIES": "/inject",
            "FELINAUT_AGENT": "1",
            "FELINAUT_CEF_PROBE_CANARY": "/outside/shell-canary",
        }
        with (
            self.execution() as state,
            patch.dict(runner.os.environ, inherited, clear=True),
            patch.object(runner, "build_binaries") as build,
            patch.object(
                runner, "compile_capture_helper", return_value=self.output / "selector"
            ) as compile_helper,
            patch.object(
                runner,
                "capture_root",
                return_value=self.output / "cef-probe-window.png",
            ) as capture,
        ):
            report = runner.execute(build=True, capture=True)
        self.assertTrue(report["success"])
        self.assertEqual(
            build.call_args.args[0], runner.sanitized_environment(inherited)
        )
        self.assertEqual(
            compile_helper.call_args.args[0], runner.sanitized_environment(inherited)
        )
        native = state["env"]
        self.assertEqual(native["HOME"], str(state["profile"].path))
        self.assertEqual(native["PATH"], "/usr/bin:/bin:/usr/sbin:/sbin")
        self.assertEqual(
            native["FELINAUT_CEF_PROBE_CANARY"],
            str(self.output / "sandbox-canary"),
        )
        for env in (
            build.call_args.args[0],
            compile_helper.call_args.args[0],
            capture.call_args.args[3],
        ):
            self.assertNotIn("FELINAUT_CEF_PROBE_CANARY", env)
        self.assertEqual(
            inherited["FELINAUT_CEF_PROBE_CANARY"], "/outside/shell-canary"
        )
        for key in (
            "GITHUB_TOKEN",
            "SSLKEYLOGFILE",
            "CARGO_HOME",
            "DEVELOPER_DIR",
            "TMPDIR",
        ):
            self.assertNotIn(key, native)
        self.assertEqual(
            capture.call_args.args[3], runner.capture_environment(inherited)
        )
        self.assertNotEqual(capture.call_args.args[3]["HOME"], native["HOME"])
        self.assertTrue(state["profile"].path.exists())

    def test_failed_audit_missing_roles_or_seatbelt_make_completed_run_fail(self):
        cases = (
            ["audit-gap", "no-helpers"]
            + ["missing-" + role for role in ("renderer", "gpu-process", "utility")]
            + ["seatbelt-" + role for role in ("renderer", "gpu-process", "utility")]
        )
        for case in cases:
            with self.subTest(case=case):
                self.output = self.base / case
                self.output.mkdir(mode=0o700)
                audit = successful_audit()
                if case == "audit-gap":
                    audit["ok"] = False
                    audit["gaps"] = ["registered helper survives deadline"]
                    audit["helpers"][0]["exit_evidence"] = "unknown"
                    audit["roles"]["renderer"]["exits_verified"] = False
                elif case == "no-helpers":
                    audit["helpers"] = []
                    for role in audit["roles"].values():
                        role["count"] = 0
                elif case.startswith("missing-"):
                    audit["roles"][case.removeprefix("missing-")]["count"] = 0
                else:
                    role = case.removeprefix("seatbelt-")
                    next(h for h in audit["helpers"] if h["role"] == role)[
                        "seatbelt"
                    ] = 0
                with self.execution(audit=audit) as state:
                    with self.assertRaises(runner.ProbeError):
                        runner.execute()
                report = json.loads((self.output / "report.json").read_text())
                self.assertFalse(report["success"])
                self.assertFalse(report["helper_audit_passed"])
                self.assertEqual(report["helper_audit"], audit)
                self.assertEqual(report["root_exit"], 0)
                self.assertTrue(report["canary_control_unchanged"])
                if case != "no-helpers":
                    self.assertTrue(report["renderer_canary_denied"])
                self.assertEqual(report["home_cleanup"], "preserved")
                self.assertTrue(report["home_retained"])
                self.assertTrue(state["profile"].path.exists())
                self.assertTrue(state["server_stopped"])
                state["observer"].finish.assert_called_once_with(deadline_seconds=3)
                self.assertEqual(state["exit_at_finish"], 0)
                if case == "audit-gap":
                    self.assertEqual(report["helper_lifecycle"], "unverified")

    def test_notification_broker_makes_otherwise_valid_audit_run_fail(self):
        audit = successful_audit()
        utility = next(h for h in audit["helpers"] if h["role"] == "utility")
        utility["subtype"] = "mac_notifications.mojom.MacNotificationProvider"
        with self.execution(audit=audit) as state:
            with self.assertRaisesRegex(runner.ProbeError, "inspect report.json"):
                runner.execute()
        report = json.loads((self.output / "report.json").read_text())
        self.assertEqual(report["helper_audit"], audit)
        self.assertTrue(report["helper_audit"]["ok"])
        self.assertTrue(all(h["seatbelt"] == 1 for h in audit["helpers"]))
        self.assertTrue(all(role["count"] > 0 for role in audit["roles"].values()))
        self.assertTrue(report["renderer_canary_denied"])
        self.assertTrue(report["canary_control_unchanged"])
        self.assertFalse(report["notification_broker_absent"])
        self.assertFalse(report["helper_audit_passed"])
        self.assertFalse(report["success"])
        self.assertEqual(report["root_exit"], 0)
        self.assertEqual(report["home_cleanup"], "preserved")
        self.assertTrue(report["home_retained"])
        self.assertTrue(state["profile"].path.exists())
        self.assertTrue(state["server_stopped"])

    def test_failed_renderer_canary_makes_otherwise_valid_audit_run_fail(self):
        record = "CEF_RENDERER_CANARY pid=43214 outcome=denied errno=1\n"
        for case, replacement in {
            "missing": "",
            "allowed": record.replace(
                "outcome=denied errno=1", "outcome=allowed errno=0"
            ),
            "error": record.replace("denied errno=1", "error errno=2"),
            "duplicate": record + record,
        }.items():
            with self.subTest(case=case):
                self.output = self.base / case
                self.output.mkdir(mode=0o700)
                with self.execution(
                    text=native_fixture().replace(record, replacement)
                ) as state:
                    with self.assertRaises(runner.ProbeError):
                        runner.execute()
                report = json.loads((self.output / "report.json").read_text())
                self.assertTrue(report["helper_audit"]["ok"])
                self.assertTrue(report["canary_control_unchanged"])
                self.assertFalse(report["renderer_canary_denied"])
                self.assertFalse(report["helper_audit_passed"])
                self.assertFalse(report["success"])
                self.assertEqual(report["root_exit"], 0)
                self.assertEqual(report["home_cleanup"], "preserved")
                self.assertTrue(report["home_retained"])
                self.assertTrue(state["profile"].path.exists())

    def test_same_content_canary_replacement_fails_report_and_retains_home(self):
        with self.execution() as state:
            finish = state["observer"].finish.side_effect

            def replace_canary(*, deadline_seconds):
                path = self.output / "sandbox-canary"
                path.rename(self.output / "original-canary")
                path.write_bytes(runner.CANARY_BYTES)
                return finish(deadline_seconds=deadline_seconds)

            state["observer"].finish.side_effect = replace_canary
            with self.assertRaises(runner.ProbeError):
                runner.execute()
        report = json.loads((self.output / "report.json").read_text())
        self.assertTrue(report["helper_audit"]["ok"])
        self.assertTrue(report["renderer_canary_denied"])
        self.assertFalse(report["canary_control_unchanged"])
        self.assertFalse(report["helper_audit_passed"])
        self.assertFalse(report["success"])
        self.assertEqual(report["home_cleanup"], "preserved")
        self.assertTrue(report["home_retained"])
        self.assertTrue(state["profile"].path.exists())
        state["observer"].finish.assert_called_once_with(deadline_seconds=3)
        self.assertEqual(state["exit_at_finish"], 0)

    def test_changed_deleted_or_symlinked_canary_writes_failed_report_and_retains_home(
        self,
    ):
        for case in ("changed", "deleted", "symlinked"):
            with self.subTest(case=case):
                self.output = self.base / case
                self.output.mkdir(mode=0o700)
                target = self.output / "symlink-target"
                target.write_bytes(runner.CANARY_BYTES)
                with self.execution() as state:
                    finish = state["observer"].finish.side_effect

                    def invalidate_canary(*, deadline_seconds):
                        path = self.output / "sandbox-canary"
                        if case == "changed":
                            path.write_bytes(b"changed canary contents\n")
                        else:
                            path.unlink()
                            if case == "symlinked":
                                path.symlink_to(target)
                        return finish(deadline_seconds=deadline_seconds)

                    state["observer"].finish.side_effect = invalidate_canary
                    with self.assertRaisesRegex(
                        runner.ProbeError, "inspect report.json"
                    ):
                        runner.execute()
                report_path = self.output / "report.json"
                report = json.loads(report_path.read_text())
                self.assertEqual(report_path.stat().st_mode & 0o777, 0o600)
                self.assertTrue(report["helper_audit"]["ok"])
                self.assertTrue(report["renderer_canary_denied"])
                self.assertFalse(report["canary_control_unchanged"])
                self.assertIsInstance(report["canary_control_error"], str)
                self.assertTrue(report["canary_control_error"])
                if case == "changed":
                    self.assertIn("changed", report["canary_control_error"])
                else:
                    self.assertIn(
                        str(self.output / "sandbox-canary"),
                        report["canary_control_error"],
                    )
                self.assertFalse(report["helper_audit_passed"])
                self.assertFalse(report["success"])
                self.assertEqual(report["root_exit"], 0)
                self.assertEqual(report["home_cleanup"], "preserved")
                self.assertTrue(report["home_retained"])
                self.assertEqual(report["home"], str(state["profile"].path))
                self.assertTrue(state["profile"].path.exists())
                self.assertTrue(state["server_stopped"])
                self.assertEqual(target.read_bytes(), runner.CANARY_BYTES)
                state["observer"].finish.assert_called_once_with(deadline_seconds=3)
                self.assertEqual(state["exit_at_finish"], 0)

    def test_oversized_missing_or_unreadable_log_writes_failed_report_and_retains_home(
        self,
    ):
        original_open = Path.open
        for case, expected_error in (
            ("oversized", runner.ProbeError),
            ("missing", FileNotFoundError),
            ("unreadable", PermissionError),
        ):
            with self.subTest(case=case):
                self.output = self.base / case
                self.output.mkdir(mode=0o700)
                native_log = self.output / "native.log"

                def open_log(path, mode="r", *args, **kwargs):
                    if path == native_log and mode == "rb":
                        if case == "missing":
                            path.unlink(missing_ok=True)
                        elif case == "unreadable":
                            raise PermissionError(
                                errno.EACCES,
                                "mock native log permission denied",
                                str(path),
                            )
                    return original_open(path, mode, *args, **kwargs)

                with (
                    self.execution(code=None, text="x" * 65) as state,
                    patch.object(runner, "MAX_LOG_BYTES", 64),
                    patch.object(Path, "open", open_log),
                    patch.object(runner, "read_log", wraps=runner.read_log) as read_log,
                ):
                    with self.assertRaises(expected_error) as error:
                        runner.execute()
                # Both monitor and the final audit attempt the same failing read.
                self.assertEqual(read_log.call_count, 2)
                self.assertTrue(
                    all(call.args == (native_log,) for call in read_log.call_args_list)
                )
                report_path = self.output / "report.json"
                report = json.loads(report_path.read_text())
                self.assertEqual(report_path.stat().st_mode & 0o777, 0o600)
                self.assertEqual(report["native_log_error"], str(error.exception))
                if case == "oversized":
                    self.assertIn(
                        "bounded inspection limit", report["native_log_error"]
                    )
                    self.assertEqual(native_log.stat().st_size, 65)
                elif case == "missing":
                    self.assertFalse(native_log.exists())
                else:
                    self.assertEqual(native_log.read_bytes(), b"x" * 65)
                self.assertTrue(report["helper_audit"]["ok"])
                self.assertTrue(report["notification_broker_absent"])
                self.assertTrue(report["canary_control_unchanged"])
                self.assertFalse(report["renderer_canary_denied"])
                self.assertFalse(report["helper_audit_passed"])
                self.assertFalse(report["success"])
                self.assertEqual(report["home_cleanup"], "preserved")
                self.assertTrue(report["home_retained"])
                self.assertEqual(report["home"], str(state["profile"].path))
                self.assertTrue(state["profile"].path.exists())
                self.assertTrue(state["server_stopped"])
                self.assertTrue(state["root"].terminated)
                self.assertEqual(report["root_exit"], -15)
                state["observer"].finish.assert_called_once_with(deadline_seconds=3)
                self.assertEqual(state["exit_at_finish"], -15)

    def test_missing_marker_preserves_home_and_reports_failure_even_with_exit_zero(
        self,
    ):
        with self.execution(text="\n".join(runner.MARKERS[:-1])) as state:
            with self.assertRaises(runner.ProbeError):
                runner.execute()
        self.assertTrue(state["server_stopped"])
        self.assertTrue(state["profile"].path.exists())
        report = json.loads((self.output / "report.json").read_text())
        self.assertFalse(report["success"])
        self.assertEqual(report["home_cleanup"], "preserved")

    def test_nonzero_native_exit_preserves_home_despite_all_completion_markers(self):
        with self.execution(code=1) as state:
            with self.assertRaisesRegex(runner.ProbeError, "not zero"):
                runner.execute()
        self.assertTrue(state["server_stopped"])
        self.assertTrue(state["profile"].path.exists())
        report = json.loads((self.output / "report.json").read_text())
        self.assertFalse(report["success"])
        self.assertEqual(report["root_exit"], 1)

    def test_timeout_stops_only_owned_root_but_keeps_home(self):
        with (
            self.execution(code=None, text="") as state,
            patch.object(
                runner, "monitor", side_effect=runner.ProbeError("60-second budget")
            ),
        ):
            with self.assertRaisesRegex(runner.ProbeError, "60-second"):
                runner.execute()
        self.assertTrue(state["root"].terminated)
        self.assertTrue(state["server_stopped"])
        self.assertTrue(state["profile"].path.exists())
        state["observer"].finish.assert_called_once_with(deadline_seconds=3)
        self.assertEqual(state["exit_at_finish"], -15)

    def test_interrupt_restores_handlers_stops_root_and_server_and_preserves_home(self):
        before = {
            signum: signal.getsignal(signum)
            for signum in (signal.SIGINT, signal.SIGTERM)
        }

        def interrupt(*args):
            signal.getsignal(signal.SIGTERM)(signal.SIGTERM, None)

        with (
            self.execution(code=None) as state,
            patch.object(runner, "monitor", side_effect=interrupt),
        ):
            with self.assertRaisesRegex(runner.ProbeError, "signal"):
                runner.execute()
        self.assertTrue(state["root"].terminated)
        self.assertTrue(state["server_stopped"])
        self.assertTrue(state["profile"].path.exists())
        state["observer"].finish.assert_called_once_with(deadline_seconds=3)
        self.assertEqual(state["exit_at_finish"], -15)
        self.assertEqual(
            before, {signum: signal.getsignal(signum) for signum in before}
        )

    def test_unconfirmed_cleanup_preserves_home_and_failure_report(self):
        with (
            self.execution(code=None) as state,
            patch.object(runner, "monitor"),
            patch.object(
                runner, "stop_owned", side_effect=runner.ProbeError("Could not stop")
            ),
        ):
            with self.assertRaisesRegex(runner.ProbeError, "Could not stop"):
                runner.execute()
        self.assertTrue(state["profile"].path.exists())
        report = json.loads((self.output / "report.json").read_text())
        self.assertFalse(report["success"])
        self.assertIsNone(report["root_exit"])

    def test_non_native_host_fails_before_any_sdk_or_process_action(self):
        with (
            patch.object(runner.platform, "system", return_value="Linux"),
            patch.object(runner.bundle_probe, "verify_prepared_sdk") as sdk,
        ):
            with self.assertRaisesRegex(runner.ProbeError, "macOS arm64"):
                runner.execute()
        sdk.assert_not_called()


if __name__ == "__main__":
    unittest.main()
