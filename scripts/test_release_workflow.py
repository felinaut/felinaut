"""Execute the workflow's architecture check against real, offline Mach-O fixtures."""

import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

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


if __name__ == "__main__":
    unittest.main()
