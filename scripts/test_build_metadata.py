"""Exercise the real Rust build script in isolated repositories; no browser required."""

import os
import subprocess
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
            ["rustc", "--edition=2024", str(ROOT / "build.rs"), "-o", str(cls.binary)],
            check=True,
            timeout=60,
        )

    @classmethod
    def tearDownClass(cls):
        cls.workspace.cleanup()

    def git(self, directory, *args):
        return subprocess.check_output(
            [
                "git",
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
            text=True,
            stderr=subprocess.PIPE,
            timeout=15,
        ).strip()

    def run_build(self, directory, env=None):
        return subprocess.check_output(
            [str(self.binary)],
            cwd=directory,
            env=env,
            text=True,
            timeout=15,
        )

    def init_repo(self, directory):
        self.git(directory, "init", "--initial-branch=main")
        self.git(directory, "commit", "--allow-empty", "-m", "Initial")
        return self.git(directory, "rev-parse", "HEAD")[:12]

    def test_archive_and_missing_git_use_unknown(self):
        with tempfile.TemporaryDirectory() as directory:
            self.assertIn("FELIFOX_GIT_COMMIT=unknown", self.run_build(directory))
            env = dict(os.environ, PATH="")
            self.assertIn("FELIFOX_GIT_COMMIT=unknown", self.run_build(directory, env))

    def test_branch_commits_and_packed_refs_are_watched(self):
        with tempfile.TemporaryDirectory() as directory:
            first = self.init_repo(directory)
            output = self.run_build(directory)
            self.assertIn(f"FELIFOX_GIT_COMMIT={first}", output)
            for path in ("HEAD", "refs/heads/main", "packed-refs"):
                self.assertIn(f"cargo:rerun-if-changed=.git/{path}\n", output)
            self.git(directory, "commit", "--allow-empty", "-m", "Second")
            second = self.git(directory, "rev-parse", "HEAD")[:12]
            self.assertNotEqual(first, second)
            self.git(directory, "pack-refs", "--all")
            self.assertIn(f"FELIFOX_GIT_COMMIT={second}", self.run_build(directory))

    def test_detached_head(self):
        with tempfile.TemporaryDirectory() as directory:
            revision = self.init_repo(directory)
            self.git(directory, "checkout", "--detach")
            output = self.run_build(directory)
            self.assertIn(f"FELIFOX_GIT_COMMIT={revision}", output)
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
            self.assertIn(f"FELIFOX_GIT_COMMIT={revision}", output)
            head = self.git(worktree, "rev-parse", "--git-path", "HEAD")
            self.assertIn(f"cargo:rerun-if-changed={head}\n", output)


if __name__ == "__main__":
    unittest.main()
