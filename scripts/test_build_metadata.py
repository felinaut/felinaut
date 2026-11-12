"""Exercise the real Rust build script in isolated repositories; no browser required."""

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


class BuildMetadataTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workspace = tempfile.TemporaryDirectory()
        cls.binary = Path(cls.workspace.name) / "build-metadata"
        subprocess.run(
            [
                "rustc",
                "--edition=2024",
                '--cfg=feature="engine-webkit-system"',
                str(ROOT / "build.rs"),
                "-o",
                str(cls.binary),
            ],
            check=True,
            env=cls.build_env({"CARGO_PKG_VERSION": None}),
            timeout=60,
        )

    @classmethod
    def tearDownClass(cls):
        cls.workspace.cleanup()

    def git(self, directory, *args):
        return subprocess.check_output(
            [
                "git",
                "--no-pager",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                *args,
            ],
            cwd=directory,
            env=self.build_env(),
            text=True,
            stderr=subprocess.PIPE,
            timeout=15,
        ).strip()

    @staticmethod
    def build_env(overrides=None):
        env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith("GIT_")
        }
        env["GIT_CONFIG_NOSYSTEM"] = "1"
        env["GIT_CONFIG_GLOBAL"] = os.devnull
        env.pop("FELINAUT_RELEASE_VERSION", None)
        env["CARGO_PKG_VERSION"] = "0.1.0"
        env["CARGO_CFG_TARGET_OS"] = "macos"
        for key, value in (overrides or {}).items():
            if value is None:
                env.pop(key, None)
            else:
                env[key] = value
        return env

    def invoke_build(self, directory, env=None):
        return subprocess.run(
            [str(self.binary)],
            cwd=directory,
            env=self.build_env(env),
            capture_output=True,
            text=True,
            timeout=15,
        )

    def run_build(self, directory, env=None):
        result = self.invoke_build(directory, env)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def assert_build_version(self, output, expected):
        versions = [
            line.removeprefix("cargo:rustc-env=FELINAUT_BUILD_VERSION=")
            for line in output.splitlines()
            if line.startswith("cargo:rustc-env=FELINAUT_BUILD_VERSION=")
        ]
        self.assertEqual(versions, [expected])
        self.assertIn("cargo:rerun-if-env-changed=FELINAUT_RELEASE_VERSION\n", output)

    def init_repo(self, directory):
        self.git(directory, "init", "--initial-branch=main")
        self.git(directory, "commit", "--allow-empty", "-m", "Initial")
        return self.git(directory, "rev-parse", "HEAD")[:12]

    def test_archive_and_missing_git_use_unknown(self):
        with tempfile.TemporaryDirectory() as directory:
            self.assertIn("FELINAUT_GIT_COMMIT=unknown", self.run_build(directory))
            for env, expected in (
                ({"PATH": ""}, "0.1.0"),
                ({"FELINAUT_RELEASE_VERSION": "0.1.42"}, "0.1.42"),
                ({"PATH": "", "FELINAUT_RELEASE_VERSION": "0.1.42"}, "0.1.42"),
            ):
                with self.subTest(env=env):
                    output = self.run_build(directory, env)
                    self.assertIn("FELINAUT_GIT_COMMIT=unknown", output)
                    self.assert_build_version(output, expected)

    def test_default_version_is_the_runtime_cargo_package_version(self):
        with tempfile.TemporaryDirectory() as directory:
            self.assert_build_version(self.run_build(directory), "0.1.0")
            self.assert_build_version(
                self.run_build(directory, {"CARGO_PKG_VERSION": "12.34.56"}),
                "12.34.56",
            )

    def test_patch_override_accepts_equal_higher_and_u64_boundaries(self):
        maximum = str((1 << 64) - 1)
        cases = (
            ("0.1.0", "0.1.0"),
            ("0.1.0", "0.1.42"),
            ("0.1.7", "0.1.7"),
            ("0.1.7", f"0.1.{maximum}"),
            (f"{maximum}.{maximum}.0", f"{maximum}.{maximum}.{maximum}"),
        )
        with tempfile.TemporaryDirectory() as directory:
            for base, override in cases:
                with self.subTest(base=base, override=override):
                    self.assert_build_version(
                        self.run_build(
                            directory,
                            {
                                "CARGO_PKG_VERSION": base,
                                "FELINAUT_RELEASE_VERSION": override,
                            },
                        ),
                        override,
                    )

    def test_invalid_override_fails_with_a_clear_build_error(self):
        invalid = (
            "",
            " ",
            "0.1",
            "0.1.7.0",
            "v0.1.7",
            "0.1.7-beta",
            "0.1.7+build",
            " 0.1.7",
            "0.1.7 ",
            "0.1.7\n",
            "0.1.7\r\n",
            "0.1.7\t",
            "00.1.7",
            "0.01.7",
            "0.1.07",
            "0.1.+7",
            "0.1.-7",
            ".1.7",
            "0..7",
            "0.1.",
            "0.1.７",
            "０.１.７",
            "0.1.٧",
            "0.1.6",
            "0.2.7",
            "1.1.7",
            "1.0.999",
            "18446744073709551616.1.7",
            "0.18446744073709551616.7",
            "0.1.18446744073709551616",
            "0.1." + "9" * 100,
            os.fsdecode(b"\xff"),
        )
        with tempfile.TemporaryDirectory() as directory:
            for override in invalid:
                with self.subTest(override=override):
                    result = self.invoke_build(
                        directory,
                        {
                            "CARGO_PKG_VERSION": "0.1.7",
                            "FELINAUT_RELEASE_VERSION": override,
                        },
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("FELINAUT_RELEASE_VERSION", result.stderr)
                    self.assertNotIn(
                        "cargo:rustc-env=FELINAUT_BUILD_VERSION=", result.stdout
                    )

    def test_invalid_or_missing_source_version_fails_clearly(self):
        with tempfile.TemporaryDirectory() as directory:
            for base in (
                None,
                "",
                "0.1",
                "0.01.0",
                "0.1.0-beta",
                "0.1.18446744073709551616",
            ):
                with self.subTest(base=base):
                    result = self.invoke_build(directory, {"CARGO_PKG_VERSION": base})
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("CARGO_PKG_VERSION", result.stderr)
                    self.assertNotIn(
                        "cargo:rustc-env=FELINAUT_BUILD_VERSION=", result.stdout
                    )

    def test_environment_is_read_fresh_on_each_invocation(self):
        with tempfile.TemporaryDirectory() as directory:
            for override in (None, "0.1.8", "0.1.2", "", None):
                with self.subTest(override=override):
                    env = {"FELINAUT_RELEASE_VERSION": override}
                    if override == "":
                        self.assertNotEqual(
                            self.invoke_build(directory, env).returncode, 0
                        )
                    else:
                        self.assert_build_version(
                            self.run_build(directory, env), override or "0.1.0"
                        )

    def test_real_release_script_cli_output_is_the_build_version(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_text(
                '[package]\nname = "fixture"\nversion = "0.1.0"\n', encoding="utf-8"
            )
            self.git(root, "init", "--initial-branch=main")
            self.git(root, "add", "Cargo.toml")
            self.git(root, "commit", "-m", "Version floor")
            self.git(root, "commit", "--allow-empty", "-m", "Next patch")
            github_output = root / "github-output"
            result = subprocess.run(
                [
                    sys.executable,
                    str(ROOT / "scripts/release_version.py"),
                    "--root",
                    str(root),
                    "--github-output",
                    str(github_output),
                ],
                env=self.build_env(),
                capture_output=True,
                text=True,
                timeout=30,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, "0.1.1\n")
            self.assertEqual(
                github_output.read_text(encoding="utf-8"), "version=0.1.1\ntag=v0.1.1\n"
            )
            output = self.run_build(
                root, {"FELINAUT_RELEASE_VERSION": result.stdout.strip()}
            )
            self.assert_build_version(output, "0.1.1")
            self.assertIn(
                f"FELINAUT_GIT_COMMIT={self.git(root, 'rev-parse', 'HEAD')[:12]}",
                output,
            )

    def test_emitted_metadata_is_embedded_without_runtime_git_or_env(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            revision = self.init_repo(root)
            output = self.run_build(root, {"FELINAUT_RELEASE_VERSION": "0.1.42"})
            compile_env = self.build_env()
            for line in output.splitlines():
                if line.startswith("cargo:rustc-env="):
                    key, value = line.removeprefix("cargo:rustc-env=").split("=", 1)
                    compile_env[key] = value
            source = root / "metadata.rs"
            source.write_text(
                'fn main() { println!("{} ({})", env!("FELINAUT_BUILD_VERSION"), '
                'env!("FELINAUT_GIT_COMMIT")); }\n',
                encoding="utf-8",
            )
            binary = root / "metadata"
            subprocess.run(
                ["rustc", "--edition=2024", str(source), "-o", str(binary)],
                env=compile_env,
                check=True,
                capture_output=True,
                timeout=60,
            )
            self.git(root, "commit", "--allow-empty", "-m", "Later commit")
            runtime_env = self.build_env(
                {
                    "PATH": "",
                    "CARGO_PKG_VERSION": "9.9.9",
                    "FELINAUT_RELEASE_VERSION": "invalid",
                    "FELINAUT_BUILD_VERSION": "9.9.9",
                    "FELINAUT_GIT_COMMIT": "runtime",
                }
            )
            result = subprocess.run(
                [str(binary)],
                cwd=root,
                env=runtime_env,
                check=True,
                capture_output=True,
                text=True,
                timeout=15,
            )
            self.assertEqual(result.stdout, f"0.1.42 ({revision})\n")

    def test_branch_commits_and_packed_refs_are_watched(self):
        with tempfile.TemporaryDirectory() as directory:
            first = self.init_repo(directory)
            output = self.run_build(directory)
            self.assertIn(f"FELINAUT_GIT_COMMIT={first}", output)
            for path in ("HEAD", "refs/heads/main", "packed-refs"):
                self.assertIn(f"cargo:rerun-if-changed=.git/{path}\n", output)
            self.git(directory, "commit", "--allow-empty", "-m", "Second")
            second = self.git(directory, "rev-parse", "HEAD")[:12]
            self.assertNotEqual(first, second)
            self.git(directory, "pack-refs", "--all")
            self.assertIn(f"FELINAUT_GIT_COMMIT={second}", self.run_build(directory))

    def test_detached_head(self):
        with tempfile.TemporaryDirectory() as directory:
            revision = self.init_repo(directory)
            self.git(directory, "checkout", "--detach")
            output = self.run_build(directory)
            self.assertIn(f"FELINAUT_GIT_COMMIT={revision}", output)
            self.assertIn("cargo:rerun-if-changed=.git/HEAD\n", output)

    def test_worktree_git_file_resolves_real_watch_paths(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory) / "repo"
            repo.mkdir()
            revision = self.init_repo(repo)
            worktree = Path(directory) / "worktree"
            self.git(repo, "worktree", "add", "--detach", str(worktree))
            self.assertTrue((worktree / ".git").is_file())
            output = self.run_build(worktree)
            self.assertIn(f"FELINAUT_GIT_COMMIT={revision}", output)
            head = self.git(worktree, "rev-parse", "--git-path", "HEAD")
            self.assertIn(f"cargo:rerun-if-changed={head}\n", output)


if __name__ == "__main__":
    unittest.main()
