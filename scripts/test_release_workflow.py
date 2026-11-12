"""Exercise release workflow Python and real, offline Mach-O architecture checks."""

import hashlib
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest
from contextlib import chdir, contextmanager
from pathlib import Path
from unittest.mock import call, patch

import release_version as release

ROOT = Path(__file__).resolve().parent.parent
BINARY = Path("dist/Felinaut.app/Contents/MacOS/felinaut")
ARCHITECTURES = ("arm64", "x86_64")


@unittest.skipUnless(sys.platform == "darwin", "requires macOS Xcode tools")
class ReleaseArchitectureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fixtures = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.fixtures.cleanup)
        cls.lipo = subprocess.check_output(
            ["xcrun", "--find", "lipo"],
            text=True,
            timeout=10,
        ).strip()
        cls.objects = {}
        for arch in ARCHITECTURES:
            output = Path(cls.fixtures.name) / f"{arch}.o"
            subprocess.run(
                [
                    "xcrun",
                    "clang",
                    "-target",
                    f"{arch}-apple-macos12.0",
                    "-x",
                    "c",
                    "-c",
                    "-o",
                    str(output),
                    "-",
                ],
                input="int fixture(void) { return 0; }\n",
                text=True,
                check=True,
                capture_output=True,
                timeout=30,
            )
            cls.objects[arch] = output

    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="felinaut workflow ")
        self.addCleanup(directory.cleanup)
        self.directory = Path(directory.name)
        self.binary = self.directory / BINARY
        self.binary.parent.mkdir(parents=True)

    def verify(self, arch):
        lines = (ROOT / ".github/workflows/release.yml").read_text().splitlines()
        commands = [line.strip() for line in lines if line.strip().startswith("lipo ")]
        self.assertEqual(len(commands), 1, "expected one workflow architecture check")
        command = shlex.split(commands[0].replace("${{ matrix.arch }}", arch))
        command[0] = self.lipo
        # Xcode 16 consumes every operand after this option as an architecture;
        # newer lipo versions also accept the broken ordering, masking CI failures.
        self.assertEqual(
            command[command.index("-verify_arch") + 1 :],
            [arch],
            "the binary must precede the variadic -verify_arch option",
        )
        return subprocess.run(
            command,
            cwd=self.directory,
            text=True,
            capture_output=True,
            timeout=10,
        )

    def test_workflow_accepts_each_matching_architecture(self):
        for arch in ARCHITECTURES:
            with self.subTest(arch=arch):
                shutil.copy2(self.objects[arch], self.binary)
                result = self.verify(arch)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_workflow_rejects_the_wrong_architecture(self):
        for actual, requested in [("arm64", "x86_64"), ("x86_64", "arm64")]:
            with self.subTest(actual=actual, requested=requested):
                shutil.copy2(self.objects[actual], self.binary)
                self.assertNotEqual(self.verify(requested).returncode, 0)

    def test_workflow_accepts_both_slices_of_a_universal_fixture(self):
        subprocess.run(
            [
                self.lipo,
                "-create",
                *map(str, self.objects.values()),
                "-output",
                str(self.binary),
            ],
            check=True,
            capture_output=True,
            timeout=10,
        )
        for arch in ARCHITECTURES:
            with self.subTest(arch=arch):
                result = self.verify(arch)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_workflow_rejects_invalid_binary(self):
        self.binary.write_bytes(b"not a Mach-O file")
        self.assertNotEqual(self.verify("arm64").returncode, 0)

    def test_workflow_rejects_missing_binary(self):
        self.assertNotEqual(self.verify("arm64").returncode, 0)


def workflow_python(job):
    """Extract a job's Python run step, excluding heredocs nested in shell steps."""
    lines = (ROOT / ".github/workflows/release.yml").read_text().splitlines()
    start = lines.index(f"  {job}:") + 1
    end = next(
        (
            i
            for i in range(start, len(lines))
            if lines[i].startswith("  ") and not lines[i].startswith("    ")
        ),
        len(lines),
    )
    blocks = []
    for i in range(start, end):
        if lines[i].strip() != "python3 - <<'PY'" or lines[i - 1].strip() != "run: |":
            continue
        indent = lines[i][: len(lines[i]) - len(lines[i].lstrip())]
        stop = lines.index(f"{indent}PY", i + 1, end)
        body = lines[i + 1 : stop]
        if any(line and not line.startswith(indent) for line in body):
            raise ValueError("Python heredoc escaped its run block")
        blocks.append("\n".join(line.removeprefix(indent) for line in body))
    if len(blocks) != 1:
        raise ValueError(f"Expected one Python heredoc in {job}, found {len(blocks)}")
    return compile(blocks[0], f"release.yml:{job}", "exec")


def publish_concurrency(context):
    """Read the job's JSON-compatible scalar settings and resolve context paths."""
    lines = (ROOT / ".github/workflows/release.yml").read_text().splitlines()
    job = lines.index("  publish:")
    start = lines.index("    concurrency:", job) + 1
    settings = {}
    for line in lines[start:]:
        if not line.startswith("      "):
            break
        key, value = line.strip().split(":", 1)
        settings[key] = json.loads(value)

    def resolve(match):
        value = context
        for part in match.group(1).strip().split("."):
            value = value[part]
        return str(value)

    settings["group"] = re.sub(r"\$\{\{([^{}]+)\}\}", resolve, settings["group"])
    return settings


class ReleaseConcurrencyTests(unittest.TestCase):
    def settings(self, tag, *, run=1, attempt=1, event="push"):
        return publish_concurrency(
            {
                "needs": {"metadata": {"outputs": {"tag": tag}}},
                "github": {
                    "run_id": run,
                    "run_attempt": attempt,
                    "event_name": event,
                    "ref": "refs/heads/main",
                },
            }
        )

    def test_same_tag_shares_a_group_across_runs_retries_and_manual_dispatches(self):
        original = self.settings("v0.1.42")["group"]
        self.assertTrue(original)
        for overrides in ({"run": 2}, {"attempt": 2}, {"event": "workflow_dispatch"}):
            with self.subTest(overrides=overrides):
                self.assertEqual(
                    self.settings("v0.1.42", **overrides)["group"], original
                )

    def test_different_release_versions_can_publish_independently(self):
        original = self.settings("v0.1.42")["group"]
        for tag in ("v0.1.43", "v0.2.0", "v1.0.0"):
            with self.subTest(tag=tag):
                self.assertNotEqual(self.settings(tag)["group"], original)

    def test_duplicate_run_does_not_cancel_the_active_publisher(self):
        self.assertIs(self.settings("v0.1.42")["cancel-in-progress"], False)


class ReleasePythonTests(unittest.TestCase):
    SOURCE_VERSION = "0.1.0"
    VERSION = "0.1.42"
    REPOSITORY = "felinaut/felinaut"
    SHA = "0123456789abcdef0123456789abcdef01234567"
    SHORT = SHA[:12]
    PAYLOAD = b"PK\x03\x04universal fixture\x00\xff\n"

    def asset(self, arch, *, version=None, short=None):
        return (
            f"Felinaut-{version or self.VERSION}-{short or self.SHORT}-macos-{arch}.zip"
        )

    @contextmanager
    def workspace(self, **environment):
        outputs = {
            ("git", "rev-parse", "HEAD"): self.SHA + "\n",
            ("git", "rev-parse", "--short=12", "HEAD"): self.SHORT + "\n",
        }
        with (
            tempfile.TemporaryDirectory(prefix="felinaut workflow ") as directory,
            chdir(directory),
            patch.dict(
                os.environ,
                {
                    "GITHUB_ENV": str(Path(directory) / "github-env"),
                    "FELINAUT_RELEASE_VERSION": self.VERSION,
                    "FELINAUT_RELEASE_TAG": f"v{self.VERSION}",
                    "GITHUB_REPOSITORY": self.REPOSITORY,
                    "GH_TOKEN": "offline-fixture-token",
                    **environment,
                },
            ),
            patch.object(sys, "path", sys.path.copy()),
            patch.object(release, "verify_release_tag", autospec=True) as verify,
            patch(
                "subprocess.check_output",
                side_effect=lambda args, **kw: outputs[tuple(args)],
            ) as git,
            patch("subprocess.run", side_effect=self.native_command) as native,
        ):
            self.git, self.native, self.verify = git, native, verify
            Path("Cargo.toml").write_text(
                f'[package]\nversion = "{self.SOURCE_VERSION}"\n'
            )
            Path(os.environ["GITHUB_ENV"]).write_text("EXISTING=value\n")
            yield

    def payload(self, arch):
        if arch == "universal":
            return self.PAYLOAD
        return b"PK\x03\x04native " + arch.encode() + b"\x00\xff\n"

    def release_payloads(self):
        return {
            self.asset(arch): self.payload(arch)
            for arch in (*ARCHITECTURES, "universal")
        }

    def native_command(self, args, **kwargs):
        if args[:3] == ["ditto", "-x", "-k"]:
            (Path(args[-1]) / "Felinaut.app").mkdir()
        elif args[:2] == ["ditto", "-c"]:
            self.assertTrue(Path("dist/Felinaut.app").is_dir())
            Path(args[-1]).write_bytes(self.payload(os.environ["ARCH"]))
        elif args[0] == "python3":
            for arch in ARCHITECTURES:
                self.assertTrue(Path(f"native-apps/{arch}/Felinaut.app").is_dir())
            self.assertEqual(list(Path("release-assets").iterdir()), [])
            Path(args[-1]).write_bytes(self.PAYLOAD)
        else:
            self.fail(f"Unexpected native command: {args}")
        return subprocess.CompletedProcess(args, 0)

    def native_inputs(self, overrides=None):
        for arch in ARCHITECTURES:
            directory = Path("native-zips") / arch
            directory.mkdir(parents=True)
            for name in (overrides or {}).get(arch, [self.asset(arch)]):
                (directory / name).write_bytes(self.payload(arch))

    def release_inputs(self, names=None):
        contents = (
            self.release_payloads()
            if names is None
            else {name: b"ZIP fixture\x00\xff" for name in names}
        )
        Path("release-assets").mkdir()
        for name, payload in contents.items():
            (Path("release-assets") / name).write_bytes(payload)
        return contents

    def assert_release_files(self, expected):
        self.assertEqual(
            {p.name: p.read_bytes() for p in Path("release-assets").iterdir()},
            expected,
        )

    def assert_publish_unchanged(self, contents):
        self.assert_release_files(contents)
        self.assertEqual(Path(os.environ["GITHUB_ENV"]).read_text(), "EXISTING=value\n")
        self.native.assert_not_called()

    def assembly_calls(self):
        return [
            call(
                [
                    "ditto",
                    "-x",
                    "-k",
                    f"native-zips/{arch}/{self.asset(arch)}",
                    f"native-apps/{arch}",
                ],
                check=True,
                timeout=120,
            )
            for arch in ARCHITECTURES
        ] + [
            call(
                [
                    "python3",
                    "scripts/assemble_universal_macos.py",
                    "--arm64",
                    "native-apps/arm64/Felinaut.app",
                    "--x86-64",
                    "native-apps/x86_64/Felinaut.app",
                    "--output",
                    f"release-assets/{self.asset('universal')}",
                ],
                check=True,
                timeout=600,
            )
        ]

    def execute(self, job):
        exec(workflow_python(job), {"__name__": "__main__"})

    def test_native_zip_uses_effective_version_and_architecture(self):
        for arch in ARCHITECTURES:
            with self.subTest(arch=arch), self.workspace(ARCH=arch):
                Path("dist/Felinaut.app").mkdir(parents=True)
                self.execute("build")
                self.native.assert_called_once_with(
                    [
                        "ditto",
                        "-c",
                        "-k",
                        "--sequesterRsrc",
                        "--keepParent",
                        "dist/Felinaut.app",
                        f"release-assets/{self.asset(arch)}",
                    ],
                    check=True,
                )
                self.git.assert_called_once_with(
                    ["git", "rev-parse", "--short=12", "HEAD"],
                    text=True,
                )
                self.assert_release_files({self.asset(arch): self.payload(arch)})
                self.verify.assert_not_called()

    def test_universal_extracts_current_native_zips_and_calls_assembler(self):
        with self.workspace():
            self.native_inputs()
            self.execute("universal")
            self.assertEqual(self.native.call_args_list, self.assembly_calls())
            self.git.assert_called_once_with(
                ["git", "rev-parse", "--short=12", "HEAD"],
                text=True,
            )
            self.assert_release_files(self.release_payloads())
            for arch in ARCHITECTURES:
                self.assertEqual(
                    (Path("native-zips") / arch / self.asset(arch)).read_bytes(),
                    self.payload(arch),
                )
            self.verify.assert_not_called()

    def test_universal_rejects_missing_stale_misnamed_or_extra_native_artifacts(self):
        for arch in ARCHITECTURES:
            cases = {
                "missing": [],
                "wrong commit": [self.asset(arch, short="deadbeef0000")],
                "source version": [self.asset(arch, version=self.SOURCE_VERSION)],
                "wrong architecture": [
                    self.asset("x86_64" if arch == "arm64" else "arm64")
                ],
                "extra": [self.asset(arch), "unexpected.txt"],
            }
            for reason, names in cases.items():
                with self.subTest(arch=arch, reason=reason), self.workspace():
                    self.native_inputs({arch: names})
                    with self.assertRaisesRegex(
                        RuntimeError, f"unexpected native {arch}"
                    ):
                        self.execute("universal")
                    for actual in self.native.call_args_list:
                        self.assertIn(actual, self.assembly_calls()[:2])
                    self.assertFalse(Path("release-assets").exists())

    def test_universal_stops_on_native_command_failure_without_a_zip(self):
        for index in range(3):
            with self.subTest(command=index), self.workspace():
                self.native_inputs()
                calls = self.assembly_calls()

                def fail_command(args, **kwargs):
                    if args == calls[index].args[0]:
                        raise subprocess.CalledProcessError(1, args)
                    return self.native_command(args, **kwargs)

                self.native.side_effect = fail_command
                with self.assertRaises(subprocess.CalledProcessError):
                    self.execute("universal")
                self.assertEqual(self.native.call_args_list, calls[: index + 1])
                self.assertEqual(list(Path("release-assets").glob("*")), [])

    def test_publish_checksums_all_three_zips_and_exports_semantic_metadata(self):
        with self.workspace():
            contents = self.release_inputs()
            self.verify.side_effect = lambda *args: self.assert_publish_unchanged(
                contents
            )
            self.execute("publish")
            self.verify.assert_called_once_with(
                self.REPOSITORY, f"v{self.VERSION}", self.SHA
            )
            checksums = "".join(
                f"{hashlib.sha256(contents[name]).hexdigest()}  {name}\n"
                for name in sorted(contents)
            )
            self.assert_release_files({**contents, "SHA256SUMS": checksums.encode()})
            self.assertEqual(
                Path(os.environ["GITHUB_ENV"]).read_text(),
                f"EXISTING=value\nRELEASE_TAG=v{self.VERSION}\n"
                f"RELEASE_TITLE=Felinaut {self.VERSION} ({self.SHORT})\n",
            )
            self.assertEqual(
                self.git.call_args_list,
                [
                    call(["git", "rev-parse", "HEAD"], text=True),
                    call(["git", "rev-parse", "--short=12", "HEAD"], text=True),
                ],
            )
            self.native.assert_not_called()

    def test_publish_rejects_missing_stale_or_extra_assets_without_outputs(self):
        expected = list(self.release_payloads())
        cases = {
            "empty": [],
            "thin pair only": expected[:2],
            "universal only": expected[2:],
            "extra non-ZIP": [*expected, "unexpected.txt"],
            "stale checksum": [*expected, "SHA256SUMS"],
        }
        for arch in (*ARCHITECTURES, "universal"):
            name = self.asset(arch)
            others = [p for p in expected if p != name]
            cases[f"missing {arch}"] = others
            cases[f"wrong commit {arch}"] = [
                *others,
                self.asset(arch, short="deadbeef0000"),
            ]
            cases[f"source version {arch}"] = [
                *others,
                self.asset(arch, version=self.SOURCE_VERSION),
            ]
            cases[f"extra ZIP {arch}"] = [
                *expected,
                self.asset(arch, short="deadbeef0000"),
            ]
        for reason, names in cases.items():
            with self.subTest(reason=reason), self.workspace():
                contents = self.release_inputs(names)
                with self.assertRaisesRegex(
                    RuntimeError, "^Missing or unexpected release assets$"
                ):
                    self.execute("publish")
                self.assert_publish_unchanged(contents)
                self.verify.assert_not_called()

    def test_publish_rejects_version_tag_disagreement_without_outputs(self):
        for tag in (f"v{self.SOURCE_VERSION}", self.VERSION, "v0.1.43"):
            with self.subTest(tag=tag), self.workspace(FELINAUT_RELEASE_TAG=tag):
                contents = self.release_inputs()
                with self.assertRaisesRegex(
                    RuntimeError, "^Release tag and version disagree$"
                ):
                    self.execute("publish")
                self.assert_publish_unchanged(contents)
                self.verify.assert_not_called()

    def test_publish_rejects_existing_tag_for_wrong_commit_without_outputs(self):
        with self.workspace():
            contents = self.release_inputs()
            self.verify.side_effect = ValueError(
                f"Existing release tag v{self.VERSION} points to {'f' * 40}, not commit {self.SHA}"
            )
            with self.assertRaisesRegex(
                ValueError, "Existing release tag .* not commit"
            ):
                self.execute("publish")
            self.verify.assert_called_once_with(
                self.REPOSITORY, f"v{self.VERSION}", self.SHA
            )
            self.assert_publish_unchanged(contents)


def workflow_publish_shell():
    """Read the actual publishing run block, preserving its shell/heredoc nesting."""
    lines = (ROOT / ".github/workflows/release.yml").read_text().splitlines()
    job = lines.index("  publish:")
    step = lines.index(
        "      - name: Create or resume draft, upload, then publish", job
    )
    start = lines.index("        run: |", step) + 1
    indent = "          "
    end = next(
        (
            i
            for i in range(start, len(lines))
            if lines[i].strip() and not lines[i].startswith(indent)
        ),
        len(lines),
    )
    return "\n".join(line.removeprefix(indent) for line in lines[start:end]) + "\n"


@unittest.skipUnless(
    shutil.which("bash") and shutil.which("python3"), "requires Bash and Python 3"
)
class ReleaseShellTests(unittest.TestCase):
    VERSION = "0.1.42"
    SHA = "0123456789abcdef0123456789abcdef01234567"

    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="felinaut publish shell ")
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.tag = f"v{self.VERSION}"
        self.title = f"Felinaut {self.VERSION} ({self.SHA[:12]})"
        self.state = self.directory / "stub-state.json"
        self.log = self.directory / "stub-calls.jsonl"
        self.bin = self.directory / "bin"
        self.bin.mkdir()
        gh = self.bin / "gh"
        gh.write_text(
            "#!/usr/bin/env python3\n"
            + textwrap.dedent("""\
                import json
                import os
                from pathlib import Path
                import sys

                args = sys.argv[1:]
                with open(os.environ['GH_STUB_LOG'], 'a') as log:
                    log.write(json.dumps(args) + '\\n')
                if args[:2] == ['release', 'view']:
                    state = json.loads(Path(os.environ['GH_STUB_STATE']).read_text())
                    if state is None:
                        sys.exit(1)
                    fields = args[args.index('--json') + 1].split(',')
                    print(json.dumps({key: state[key] for key in fields}))
                elif args[:2] not in (
                    ['release', 'create'], ['release', 'upload'], ['release', 'edit']
                ):
                    sys.exit('Unexpected offline gh command: ' + repr(args))
                """)
        )
        gh.chmod(0o755)
        assets = self.directory / "release-assets"
        assets.mkdir()
        self.zips = sorted(
            f"release-assets/Felinaut-{self.VERSION}-{self.SHA[:12]}-macos-{arch}.zip"
            for arch in (*ARCHITECTURES, "universal")
        )
        checksums = []
        for name in self.zips:
            payload = b"offline ZIP fixture: " + name.encode()
            (self.directory / name).write_bytes(payload)
            checksums.append(
                f"{hashlib.sha256(payload).hexdigest()}  {Path(name).name}\n"
            )
        (assets / "SHA256SUMS").write_text("".join(checksums))

    def run_publish(self, state):
        self.state.write_text(json.dumps(state))
        env = os.environ.copy()
        # Do not source a caller's shell startup file or use their gh configuration.
        for key in list(env):
            if key in ("BASH_ENV", "ENV") or key.startswith("BASH_FUNC_"):
                env.pop(key)
        env.update(
            PATH=str(self.bin) + os.pathsep + env.get("PATH", os.defpath),
            GH_CONFIG_DIR=str(self.directory / "gh-config"),
            GH_TOKEN="offline-fixture-token",
            GH_REPO="fixture/repository",
            GH_STUB_STATE=str(self.state),
            GH_STUB_LOG=str(self.log),
            GITHUB_SHA=self.SHA,
            GITHUB_ENV=str(self.directory / "github-env"),
            RELEASE_TAG=self.tag,
            RELEASE_TITLE=self.title,
        )
        result = subprocess.run(
            [
                shutil.which("bash"),
                "--noprofile",
                "--norc",
                "-e",
                "-o",
                "pipefail",
                "-c",
                workflow_publish_shell(),
            ],
            cwd=self.directory,
            env=env,
            capture_output=True,
            text=True,
            timeout=15,
        )
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        return result, calls

    def view_call(self):
        return ["release", "view", self.tag, "--json", "isDraft,targetCommitish"]

    def upload_call(self):
        return [
            "release",
            "upload",
            self.tag,
            *self.zips,
            "release-assets/SHA256SUMS",
            "--clobber",
        ]

    def publish_call(self):
        return [
            "release",
            "edit",
            self.tag,
            "--draft=false",
            "--prerelease",
            "--latest=false",
        ]

    def test_same_commit_draft_uploads_all_three_zips_then_publishes(self):
        result, calls = self.run_publish({"isDraft": True, "targetCommitish": self.SHA})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            calls, [self.view_call(), self.upload_call(), self.publish_call()]
        )

    def test_wrong_commit_draft_fails_before_upload_or_publish(self):
        result, calls = self.run_publish({"isDraft": True, "targetCommitish": "f" * 40})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not target this exact source commit", result.stderr)
        self.assertEqual(calls, [self.view_call()])

    def test_branch_target_draft_fails_before_upload_or_publish(self):
        result, calls = self.run_publish({"isDraft": True, "targetCommitish": "main"})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not target this exact source commit", result.stderr)
        self.assertEqual(calls, [self.view_call()])

    def test_second_publisher_leaves_the_first_publishers_release_unchanged(self):
        first, first_calls = self.run_publish(None)
        self.assertEqual(first.returncode, 0, first.stderr)
        second, calls = self.run_publish(
            {"isDraft": False, "targetCommitish": self.SHA}
        )
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(calls[len(first_calls) :], [self.view_call()])

    def test_published_release_exits_without_overwriting_assets(self):
        result, calls = self.run_publish({"isDraft": False, "targetCommitish": "main"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("leaving its assets unchanged", result.stdout)
        self.assertEqual(calls, [self.view_call()])

    def test_missing_release_creates_exact_target_then_uploads_and_publishes(self):
        result, calls = self.run_publish(None)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            calls,
            [
                self.view_call(),
                [
                    "release",
                    "create",
                    self.tag,
                    "--target",
                    self.SHA,
                    "--title",
                    self.title,
                    "--draft",
                    "--prerelease",
                    "--generate-notes",
                    "--notes-file",
                    "release-notes.md",
                ],
                self.upload_call(),
                self.publish_call(),
            ],
        )
        self.assertTrue((self.directory / "release-notes.md").is_file())


if __name__ == "__main__":
    unittest.main()
