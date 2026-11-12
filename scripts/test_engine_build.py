"""Exercise actual build.rs feature gates without downloading or launching an engine."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CANDIDATES = ("cef", "gecko", "servo", "ladybird", "ultralight", "wpe")


class EngineBuildTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workspace = tempfile.TemporaryDirectory()
        cls.binaries = {}

    @classmethod
    def tearDownClass(cls):
        cls.workspace.cleanup()

    def invoke(self, features, target="macos"):
        features = tuple(features)
        if features not in self.binaries:
            binary = Path(self.workspace.name) / f"build-{len(self.binaries)}"
            command = [
                "rustc",
                "--edition=2024",
                str(ROOT / "build.rs"),
                "-o",
                str(binary),
            ]
            command.extend(f'--cfg=feature="{feature}"' for feature in features)
            subprocess.run(command, check=True, capture_output=True, timeout=60)
            self.binaries[features] = binary
        environment = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith(("CARGO_", "GIT_", "FELINAUT_RELEASE_VERSION"))
        }
        environment.update(
            CARGO_PKG_VERSION="0.1.0",
            CARGO_CFG_TARGET_OS=target,
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=os.devnull,
        )
        return subprocess.run(
            [str(self.binaries[features])],
            cwd=self.workspace.name,
            env=environment,
            capture_output=True,
            text=True,
            timeout=15,
        )

    def test_single_or_both_webkit_features_build_successfully(self):
        for features in (
            ["engine-webkit-system"],
            ["engine-webkit-upstream"],
            ["engine-webkit-system", "engine-webkit-upstream"],
        ):
            with self.subTest(features=features):
                result = self.invoke(features)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(
                    "cargo:rustc-env=FELINAUT_BUILD_VERSION=0.1.0", result.stdout
                )

    def test_no_engine_or_internal_dependency_feature_alone_fails(self):
        for features in ([], ["_cocoa-webkit"]):
            with self.subTest(features=features):
                result = self.invoke(features)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Select at least one implemented engine", result.stderr)
                self.assertNotIn(
                    "cargo:rustc-env=FELINAUT_BUILD_VERSION=", result.stdout
                )

    def test_each_unimplemented_adapter_fails_instead_of_using_webkit(self):
        for candidate in CANDIDATES:
            for prefix in ([], ["engine-webkit-system"]):
                features = prefix + [f"engine-{candidate}"]
                with self.subTest(features=features):
                    result = self.invoke(features)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(candidate, result.stderr)
                    self.assertIn("not implemented", result.stderr)
                    self.assertNotIn(
                        "cargo:rustc-env=FELINAUT_BUILD_VERSION=", result.stdout
                    )

    def test_native_platform_support_cannot_be_enabled_by_a_feature(self):
        for target in ("linux", "windows", ""):
            with self.subTest(target=target):
                result = self.invoke(["engine-webkit-system"], target)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("No native Felinaut adapter implemented", result.stderr)


if __name__ == "__main__":
    unittest.main()
