"""Exercise actual build.rs feature gates without downloading or launching an engine."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
UNIMPLEMENTED_CANDIDATES = ("gecko", "servo", "ladybird", "ultralight", "wpe")
WEBKIT_FEATURES = (
    ("engine-webkit-system",),
    ("engine-webkit-upstream",),
    ("engine-webkit-system", "engine-webkit-upstream"),
)
CEF_FEATURES = (
    ("engine-cef",),
    ("engine-webkit-system", "engine-cef"),
    ("engine-webkit-upstream", "engine-cef"),
    ("engine-webkit-system", "engine-webkit-upstream", "engine-cef"),
)


class EngineBuildTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workspace = tempfile.TemporaryDirectory()
        cls.binaries = {}

    @classmethod
    def tearDownClass(cls):
        cls.workspace.cleanup()

    def invoke(self, features, target="macos", arch="aarch64"):
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
        if arch is not None:
            environment["CARGO_CFG_TARGET_ARCH"] = arch
        return subprocess.run(
            [str(self.binaries[features])],
            cwd=self.workspace.name,
            env=environment,
            capture_output=True,
            text=True,
            timeout=15,
        )

    def test_single_or_both_webkit_features_build_successfully(self):
        for features in WEBKIT_FEATURES:
            for arch in ("aarch64", "x86_64"):
                with self.subTest(features=features, arch=arch):
                    result = self.invoke(features, arch=arch)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIn(
                        "cargo:rustc-env=FELINAUT_BUILD_VERSION=0.1.0", result.stdout
                    )

    def test_cef_only_or_mixed_features_build_successfully_on_macos_arm64(self):
        for features in CEF_FEATURES:
            with self.subTest(features=features):
                result = self.invoke(features, target="macos", arch="aarch64")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(
                    "cargo:rustc-env=FELINAUT_BUILD_VERSION=0.1.0", result.stdout
                )

    def test_cef_requires_explicit_native_arm64_architecture(self):
        for features in CEF_FEATURES:
            for arch in ("x86_64", "arm", "", None):
                with self.subTest(features=features, arch=arch):
                    result = self.invoke(features, arch=arch)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("CEF runtime", result.stderr)
                    self.assertIn("requires native macOS arm64", result.stderr)
                    self.assertNotIn(
                        "cargo:rustc-env=FELINAUT_BUILD_VERSION=", result.stdout
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
        for candidate in UNIMPLEMENTED_CANDIDATES:
            for prefix in (
                [],
                ["engine-webkit-system"],
                ["engine-cef"],
                ["engine-webkit-system", "engine-cef"],
            ):
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
            for features in WEBKIT_FEATURES + CEF_FEATURES:
                with self.subTest(target=target, features=features):
                    result = self.invoke(features, target, arch="aarch64")
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(
                        "No native Felinaut adapter implemented", result.stderr
                    )
                    self.assertNotIn(
                        "cargo:rustc-env=FELINAUT_BUILD_VERSION=", result.stdout
                    )


if __name__ == "__main__":
    unittest.main()
