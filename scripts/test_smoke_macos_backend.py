"""Offline behavioral tests: no browser, GUI, Swift compiler or desktop capture."""

import contextlib
import copy
import http.client
import importlib.util
import io
import json
import os
import stat
import struct
import subprocess
import tempfile
import unittest
import zlib
from pathlib import Path
from unittest.mock import Mock, call, patch

import smoke_macos_backend as smoke


def chunk(kind, payload=b""):
    return (
        struct.pack(">I", len(payload))
        + kind
        + payload
        + struct.pack(">I", zlib.crc32(kind + payload))
    )


HEADER = chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
IMAGE = chunk(b"IDAT", zlib.compress(b"\0\x10\x20\x30"))
END = chunk(b"IEND")
PNG = smoke.PNG_SIGNATURE + HEADER + IMAGE + END
BACKEND = {
    "backend": "native-webkit",
    "engine_family": "webkit",
    "runtime_version": "623.1.14",
    "capabilities": {
        "zoom": True,
        "audio_control": True,
        "trusted_input": True,
        "snapshot": True,
        "certificate_exceptions": True,
    },
}


class PngTests(unittest.TestCase):
    def test_image_and_color_chunks_are_preserved_byte_for_byte(self):
        color = chunk(b"sRGB", b"\0") + chunk(b"gAMA", struct.pack(">I", 45455))
        original = smoke.PNG_SIGNATURE + HEADER + color + IMAGE + END
        self.assertEqual(smoke.strip_png_metadata(original), original)

    def test_all_incidental_metadata_is_removed_before_and_after_image(self):
        metadata = b"".join(
            chunk(kind, b"incidental details") for kind in sorted(smoke.PNG_METADATA)
        )
        original = smoke.PNG_SIGNATURE + HEADER + metadata + IMAGE + metadata + END
        self.assertEqual(smoke.strip_png_metadata(original), PNG)
        self.assertEqual(smoke.strip_png_metadata(PNG), PNG)

    def test_every_truncated_prefix_is_rejected(self):
        for end in range(len(PNG)):
            with self.subTest(end=end), self.assertRaises(smoke.SmokeError):
                smoke.strip_png_metadata(PNG[:end])

    def test_bad_signature_and_chunk_crcs_are_rejected(self):
        with self.assertRaisesRegex(smoke.SmokeError, "signature"):
            smoke.strip_png_metadata(b"not png" + PNG[8:])
        for position in (20, len(PNG) - 1):
            corrupt = bytearray(PNG)
            corrupt[position] ^= 1
            with (
                self.subTest(position=position),
                self.assertRaisesRegex(smoke.SmokeError, "CRC"),
            ):
                smoke.strip_png_metadata(bytes(corrupt))
        # Discarded metadata must still have a valid CRC.
        metadata = chunk(b"tEXt", b"Author\0private")[:-1] + b"\xff"
        with self.assertRaisesRegex(smoke.SmokeError, "CRC"):
            smoke.strip_png_metadata(
                smoke.PNG_SIGNATURE + HEADER + metadata + IMAGE + END
            )

    def test_malformed_chunk_structure_is_rejected(self):
        bad_streams = [
            IMAGE + END,  # no IHDR
            HEADER + HEADER + IMAGE + END,  # duplicate IHDR
            chunk(b"IHDR", b"short") + IMAGE + END,
            chunk(b"IHDR", struct.pack(">IIBBBBB", 0, 1, 8, 2, 0, 0, 0)) + IMAGE + END,
            HEADER + END,  # no image
            HEADER + IMAGE,  # no IEND
            HEADER + IMAGE + chunk(b"IEND", b"x"),
            HEADER + IMAGE + END + b"private trailing bytes",
            HEADER + chunk(b"te1t") + IMAGE + END,
            HEADER + chunk(b"texT") + IMAGE + END,  # reserved bit
            HEADER + struct.pack(">I", 0xFFFFFFFF) + b"tEXt" + b"\0" * 4,
        ]
        for stream in bad_streams:
            with self.subTest(stream=stream), self.assertRaises(smoke.SmokeError):
                smoke.strip_png_metadata(smoke.PNG_SIGNATURE + stream)

    def test_file_is_only_rewritten_after_successful_validation(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "capture.png"
            path.write_bytes(b"broken")
            with self.assertRaises(smoke.SmokeError):
                smoke.clean_png(path)
            self.assertEqual(path.read_bytes(), b"broken")
            path.write_bytes(
                smoke.PNG_SIGNATURE + HEADER + chunk(b"tEXt", b"private") + IMAGE + END
            )
            smoke.clean_png(path)
            self.assertEqual(path.read_bytes(), PNG)


class SnapshotTests(unittest.TestCase):
    def test_exact_role_and_label_with_optional_value(self):
        snapshot = (
            "http://127.0.0.1/ | Local engine check\n"
            '@e0 textbox "Test input extra"\n'
            '@e12 textbox "Test input" = "Hello"\n'
            '@e3 button "Verify input"\n'
        )
        self.assertEqual(smoke.snapshot_ref(snapshot, "textbox", "Test input"), "@e12")
        self.assertEqual(smoke.snapshot_ref(snapshot, "button", "Verify input"), "@e3")

    def test_labels_are_literal_and_json_escaped(self):
        label = 'A [test].* "é"\nnext'
        line = "@e42 textbox " + json.dumps(label, ensure_ascii=False)
        self.assertEqual(smoke.snapshot_ref(line, "textbox", label), "@e42")

    def test_missing_malformed_or_ambiguous_refs_fail(self):
        for snapshot in (
            "",
            '@eX textbox "Test input"',
            'prefix @e1 textbox "Test input"',
            '@e1 textbox "Test input" suffix',
            '@e1 button "Test input"',
            '@e1 textbox "Test input"\n@e2 textbox "Test input"',
        ):
            with self.subTest(snapshot=snapshot), self.assertRaises(smoke.SmokeError):
                smoke.snapshot_ref(snapshot, "textbox", "Test input")


class DiagnosticsTests(unittest.TestCase):
    def test_valid_backend_and_additional_diagnostics_are_accepted(self):
        info = copy.deepcopy(BACKEND)
        info["additional"] = "future field"
        self.assertEqual(smoke.validate_backend(info), info)

    def test_invalid_backend_diagnostics_are_rejected(self):
        invalid = [None, [], "native-webkit", {}, {"backend": "native-webkit"}]
        for field, value in (
            ("backend", "other"),
            ("engine_family", "other"),
            ("runtime_version", None),
            ("runtime_version", "  "),
            ("runtime_version", 123),
            ("capabilities", []),
        ):
            info = copy.deepcopy(BACKEND)
            info[field] = value
            invalid.append(info)
        for name in BACKEND["capabilities"]:
            for value in (False, 1, "true", None):
                info = copy.deepcopy(BACKEND)
                info["capabilities"][name] = value
                invalid.append(info)
            info = copy.deepcopy(BACKEND)
            del info["capabilities"][name]
            invalid.append(info)
        for info in invalid:
            with self.subTest(info=info), self.assertRaises(smoke.SmokeError):
                smoke.validate_backend(info)

    def test_single_tab_requires_exact_url_and_completed_loading(self):
        url = "http://127.0.0.1:1234/"
        tab = {"url": url, "loading": False}
        self.assertTrue(smoke.single_tab_at([tab], url, loaded=True))
        self.assertTrue(smoke.single_tab_at([{"url": url}], url))
        for tabs in (
            None,
            [],
            [tab, tab],
            [None],
            [{"url": "https://example.org/"}],
            [{"url": url}],
            [{"url": url, "loading": True}],
            [{"url": url, "loading": 0}],
        ):
            with self.subTest(tabs=tabs):
                self.assertFalse(smoke.single_tab_at(tabs, url, loaded=True))


class SafetyTests(unittest.TestCase):
    def test_import_has_no_launch_build_or_filesystem_side_effects(self):
        spec = importlib.util.spec_from_file_location(
            "smoke_import_check", smoke.__file__
        )
        module = importlib.util.module_from_spec(spec)
        with (
            patch.object(subprocess, "Popen") as launch,
            patch.object(subprocess, "run") as run,
            patch.object(tempfile, "mkdtemp") as directory,
            patch.object(Path, "mkdir") as mkdir,
        ):
            spec.loader.exec_module(module)
        launch.assert_not_called()
        run.assert_not_called()
        directory.assert_not_called()
        mkdir.assert_not_called()

    def test_run_opt_in_and_platform_are_checked_before_creating_output(self):
        with (
            patch.object(smoke, "create_output") as output,
            patch.object(smoke, "smoke") as launch,
            contextlib.redirect_stderr(io.StringIO()),
        ):
            for arguments in ([], ["--capture"], ["--output", "unused"]):
                with (
                    self.subTest(arguments=arguments),
                    self.assertRaises(SystemExit) as error,
                ):
                    smoke.main(arguments)
                self.assertEqual(error.exception.code, 2)
            with (
                patch.object(smoke.sys, "platform", "linux"),
                self.assertRaises(SystemExit) as error,
            ):
                smoke.main(["--run"])
            self.assertEqual(error.exception.code, 2)
        output.assert_not_called()
        launch.assert_not_called()

    def test_output_is_new_private_and_never_overwrites(self):
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.object(smoke, "ROOT", Path(temporary)),
        ):
            first = smoke.create_output(None)
            second = smoke.create_output(None)
            explicit = smoke.create_output(Path(temporary) / "chosen")
            self.assertNotEqual(first, second)
            self.assertEqual(first.parent, Path(temporary) / "target")
            for path in (first, second, explicit):
                self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o700)
                with self.assertRaises(FileExistsError):
                    smoke.create_output(path)
            file = Path(temporary) / "existing-file"
            file.write_text("keep")
            link = Path(temporary) / "link"
            link.symlink_to(Path(temporary) / "nonexistent")
            for path in (file, link):
                with self.assertRaises(FileExistsError):
                    smoke.create_output(path)
            self.assertEqual(file.read_text(), "keep")
            self.assertFalse((Path(temporary) / "nonexistent").exists())

    def test_child_home_is_isolated_without_modifying_parent_environment(self):
        with tempfile.TemporaryDirectory() as temporary:
            home = Path(temporary)
            original = dict(os.environ)
            env = smoke.isolated_environment(home)
            self.assertEqual(os.environ, original)
            self.assertEqual(env["HOME"], str(home))
            self.assertEqual(env["CFFIXED_USER_HOME"], str(home))
            self.assertEqual(env["FELINAUT_AGENT"], "1")
            self.assertEqual(
                sorted(
                    p.name
                    for p in (home / "Library/Application Support/Felifox").iterdir()
                ),
                ["settings"],
            )
            self.assertEqual(
                (home / "Library/Application Support/Felifox/settings").read_text(),
                "theme=milky-way\nanimate=false\nsidebar=true\n",
            )

    def test_loopback_fixture_is_served_and_closed_even_on_failure(self):
        connection = None
        with self.assertRaisesRegex(RuntimeError, "deliberate"):
            with smoke.fixture_server() as url:
                host = url.removeprefix("http://").removesuffix("/")
                self.assertTrue(host.startswith("127.0.0.1:"))
                connection = http.client.HTTPConnection(host, timeout=2)
                connection.request("GET", "/")
                response = connection.getresponse()
                self.assertEqual(response.status, 200)
                self.assertEqual(response.getheader("Cache-Control"), "no-store")
                self.assertEqual(
                    response.read(),
                    (smoke.ROOT / "scripts/fixtures/native_backend.html").read_bytes(),
                )
                connection.request("GET", "/../../etc/passwd")
                response = connection.getresponse()
                self.assertEqual(response.status, 404)
                response.read()
                connection.close()
                raise RuntimeError("deliberate")
        connection = http.client.HTTPConnection(host, timeout=2)
        try:
            with self.assertRaises(OSError):
                connection.connect()
        finally:
            connection.close()

    def test_subprocess_failures_are_actionable_and_bounded(self):
        failures = (
            (subprocess.TimeoutExpired("tool", 3), "timed out after 3s"),
            (
                subprocess.CalledProcessError(1, "tool", stderr="permission denied"),
                "permission denied",
            ),
            (FileNotFoundError("missing tool"), "Cannot run tool"),
        )
        for failure, message in failures:
            with (
                self.subTest(failure=failure),
                patch.object(smoke.subprocess, "run", side_effect=failure) as run,
                self.assertRaisesRegex(smoke.SmokeError, message),
            ):
                smoke.run("tool", "argument with spaces", timeout=3)
            self.assertEqual(run.call_args.args[0], ["tool", "argument with spaces"])
            self.assertEqual(run.call_args.kwargs["timeout"], 3)
            self.assertNotIn("shell", run.call_args.kwargs)

    def test_readiness_is_bounded_and_reports_last_failure(self):
        process = Mock()
        process.poll.return_value = None
        check = Mock(side_effect=smoke.SmokeError("socket not ready"))
        with (
            patch.object(smoke.time, "monotonic", side_effect=[0, 0, 1, 1]),
            patch.object(smoke.time, "sleep"),
            self.assertRaisesRegex(smoke.SmokeError, "socket not ready"),
        ):
            smoke.wait_for(process, check, "control socket", timeout=1)
        check.assert_called_once_with(1)

    def test_browser_exit_during_readiness_fails_immediately(self):
        process = Mock()
        process.poll.return_value = 9
        check = Mock()
        with self.assertRaisesRegex(smoke.SmokeError, "Browser exited"):
            smoke.wait_for(process, check, "control socket")
        check.assert_not_called()

    def test_stop_escalates_only_the_launched_process(self):
        process = Mock()
        process.poll.return_value = None
        process.wait.side_effect = [subprocess.TimeoutExpired("browser", 8), 0]
        smoke.stop_browser(process)
        process.terminate.assert_called_once_with()
        process.kill.assert_called_once_with()
        self.assertEqual(
            process.wait.call_args_list, [call(timeout=8), call(timeout=5)]
        )
        process.reset_mock()
        process.poll.return_value = 0
        smoke.stop_browser(process)
        process.terminate.assert_not_called()
        process.kill.assert_not_called()

    def test_capture_uses_only_selected_window_and_strips_metadata(self):
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "window.png"
            process = Mock(pid=123)
            process.poll.return_value = None

            def fake_run(*args, **kwargs):
                if str(args[0]) == "helper":
                    self.assertEqual(args[1], "123")
                    return "456"
                self.assertEqual(
                    args[:5], ("/usr/sbin/screencapture", "-x", "-o", "-l", "456")
                )
                self.assertEqual(args[5], destination)
                destination.write_bytes(
                    smoke.PNG_SIGNATURE
                    + HEADER
                    + chunk(b"tEXt", b"private")
                    + IMAGE
                    + END
                )
                return ""

            with (
                patch.object(smoke, "run", side_effect=fake_run) as run,
                patch.object(smoke.time, "sleep"),
            ):
                smoke.capture_window(process, Path("helper"), destination, {})
            self.assertEqual(run.call_count, 2)
            self.assertEqual(destination.read_bytes(), PNG)

    def test_invalid_window_id_never_falls_back_to_display_capture(self):
        process = Mock(pid=123)
        process.poll.return_value = None
        for window in ("", "0", "-1", "123\n456", "not a window"):
            with (
                self.subTest(window=window),
                patch.object(smoke, "run", return_value=window) as run,
                self.assertRaisesRegex(smoke.SmokeError, "window ID"),
            ):
                smoke.capture_window(process, Path("helper"), Path("unused.png"), {})
            run.assert_called_once()


class WorkflowTests(unittest.TestCase):
    """Exercise the real orchestration with fake binaries, not a running GUI."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.output = self.root / "output"
        self.output.mkdir(mode=0o700)
        binaries = self.root / "target/debug"
        binaries.mkdir(parents=True)
        for name in ("felinaut", "felinaut-ctl"):
            path = binaries / name
            path.touch()
            path.chmod(0o700)
        self.process = Mock(pid=123)
        self.process.poll.return_value = None
        self.commands = []
        self.environments = []
        self.tab_requests = 0
        self.backend_requests = 0
        self.server_live = False
        self.fail_command = None
        self.failure = smoke.SmokeError("deliberate failure")
        self.change_backend = False

    @contextlib.contextmanager
    def server(self):
        self.server_live = True
        try:
            yield "http://127.0.0.1:12345/"
        finally:
            self.server_live = False

    def fake_run(self, *args, env=None, timeout=None):
        command = tuple(str(arg) for arg in args[1:])
        self.commands.append(command)
        self.environments.append(env)
        self.assertTrue(self.server_live)
        self.assertGreater(timeout, 0)
        if command == self.fail_command:
            raise self.failure
        if command == ("backend",):
            self.backend_requests += 1
            info = copy.deepcopy(BACKEND)
            if self.change_backend and self.backend_requests == 2:
                info["runtime_version"] = "different"
            return json.dumps(info)
        if command == ("tabs",):
            self.tab_requests += 1
            return json.dumps(
                [
                    {
                        "url": "felinaut://newtab"
                        if self.tab_requests == 1
                        else "http://127.0.0.1:12345/",
                        "loading": False,
                    }
                ]
            )
        if command == ("snapshot",):
            return '@e0 textbox "Test input" = ""\n@e1 button "Verify input"'
        if command[0] == "eval":
            return json.dumps(
                {
                    "text": "Native input verified: Hello from Felinaut",
                    "trusted": "true",
                }
            )
        if command[0] == "screenshot":
            Path(command[1]).write_bytes(
                smoke.PNG_SIGNATURE + HEADER + chunk(b"eXIf", b"private") + IMAGE + END
            )
        return ""

    @contextlib.contextmanager
    def harness(self):
        with (
            patch.object(smoke, "ROOT", self.root),
            patch.object(smoke, "fixture_server", self.server),
            patch.object(smoke, "run", side_effect=self.fake_run),
            patch.object(
                smoke.subprocess, "Popen", return_value=self.process
            ) as launch,
        ):
            yield launch

    def assert_cleaned_up(self, launch):
        self.process.terminate.assert_called_once_with()
        self.process.wait.assert_called_once_with(timeout=8)
        self.assertFalse(self.server_live)
        env = launch.call_args.kwargs["env"]
        self.assertEqual(env["HOME"], env["CFFIXED_USER_HOME"])
        self.assertEqual(env["FELINAUT_AGENT"], "1")
        self.assertLess(
            len(
                os.fsencode(
                    env["HOME"] + "/Library/Application Support/Felifox/agent.sock"
                )
            ),
            104,
        )
        self.assertFalse(Path(env["HOME"]).exists())
        self.assertTrue(launch.call_args.kwargs["stdout"].closed)

    def test_success_runs_full_workflow_without_desktop_capture(self):
        with self.harness() as launch:
            report = smoke.smoke(self.output, capture=False)
        self.assert_cleaned_up(launch)
        self.assertEqual(
            launch.call_args.args[0], [str(self.root / "target/debug/felinaut")]
        )
        self.assertEqual(report["backend"], BACKEND)
        self.assertFalse(report["window_capture"])
        self.assertEqual(report["page_snapshot_bytes"], len(PNG))
        self.assertEqual(json.loads((self.output / "result.json").read_text()), report)
        self.assertEqual((self.output / "page-snapshot.png").read_bytes(), PNG)
        self.assertIn(("navigate", "http://127.0.0.1:12345/"), self.commands)
        self.assertIn(("--native", "type", "@e0", "Hello from Felinaut"), self.commands)
        self.assertIn(("--native", "click", "@e1"), self.commands)
        self.assertEqual(self.commands.count(("snapshot",)), 3)
        self.assertEqual(self.commands.count(("backend",)), 2)
        self.assertTrue(
            all(env == launch.call_args.kwargs["env"] for env in self.environments)
        )

    def test_action_failure_and_interrupt_always_clean_up(self):
        for failure in (smoke.SmokeError("deliberate failure"), KeyboardInterrupt()):
            with self.subTest(failure=type(failure).__name__):
                self.process.reset_mock()
                self.fail_command = ("--native", "type", "@e0", "Hello from Felinaut")
                self.failure = failure
                self.tab_requests = 0
                with self.harness() as launch, self.assertRaises(type(failure)):
                    smoke.smoke(self.output, capture=False)
                self.assert_cleaned_up(launch)
                self.assertFalse((self.output / "result.json").exists())

    def test_changed_backend_diagnostics_fail_and_clean_up(self):
        self.change_backend = True
        with (
            self.harness() as launch,
            self.assertRaisesRegex(smoke.SmokeError, "changed"),
        ):
            smoke.smoke(self.output, capture=False)
        self.assert_cleaned_up(launch)
        self.assertFalse((self.output / "result.json").exists())

    def test_launch_failure_stops_server_and_removes_temporary_home(self):
        with self.harness() as launch:
            launch.side_effect = OSError("cannot launch")
            with self.assertRaisesRegex(OSError, "cannot launch"):
                smoke.smoke(self.output, capture=False)
        self.assertFalse(self.server_live)
        self.assertFalse(Path(launch.call_args.kwargs["env"]["HOME"]).exists())
        self.process.terminate.assert_not_called()

    def test_missing_binary_is_actionable_and_never_launches(self):
        (self.root / "target/debug/felinaut-ctl").unlink()
        with (
            self.harness() as launch,
            self.assertRaisesRegex(smoke.SmokeError, "cargo build --locked --bins"),
        ):
            smoke.smoke(self.output, capture=False)
        launch.assert_not_called()
        self.assertFalse(self.server_live)


if __name__ == "__main__":
    unittest.main()
