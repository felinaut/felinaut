"""Offline behavior and rendering tests for the reproducible Felinaut boards.

Structural/generation tests need only Python 3.11+. PNG raster tests require
rsvg-convert on PATH and are explicitly skipped if it is unavailable.
"""

import copy
import hashlib
import re
import shutil
import struct
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path
from unittest.mock import patch

import render_brand_references as brand

ROOT = Path(__file__).resolve().parent.parent
SVG = f"{{{brand.NS}}}"
LEGACY = {
    "ChatGPT Image Nov 6, 2026 at 11_00_41 PM-1.png": "992e8b1c09ecaf09f4c10dbc7cbd58c20fe3bb5e632b1b8c829f015299407c72",
    "ChatGPT Image Nov 6, 2026 at 11_00_42 PM-2.png": "291f8461c614bf70a27655eda8e97e078284f633353740c695a42d8720816ee4",
    "ChatGPT Image Nov 6, 2026 at 11_00_43 PM-3.png": "0f267c594b7ed30e46a6e3413e5993e1fbfa340e284e2d219de5751e4d6f58fa",
    "ChatGPT Image Nov 6, 2026 at 11_00_43 PM-4.png": "b13cf4df28382fcf3c160f5a356d448205325f1e7df0425eb837e603edfde023",
}


def signature(node):
    """Normalize only placement, ID namespaces, and whitespace—not geometry."""
    node = copy.deepcopy(node)
    for attr in ("x", "y", "width", "height", "data-source"):
        node.attrib.pop(attr, None)
    ids = {
        item.get("id"): f"paint-{index}"
        for index, item in enumerate(node.iter())
        if item.get("id")
    }
    result = []
    for item in node.iter():
        attrs = dict(item.attrib)
        if "id" in attrs:
            attrs["id"] = ids[attrs["id"]]
        for key, value in attrs.items():
            attrs[key] = re.sub(
                r"url\(#([^)]*)\)", lambda m: f"url(#{ids[m[1]]})", value
            )
        result.append((item.tag, sorted(attrs.items()), (item.text or "").strip()))
    return result


class PaletteTests(unittest.TestCase):
    def setUp(self):
        self.source = (ROOT / "src/theme.rs").read_text(encoding="utf-8")

    def test_reads_all_six_current_names_keys_and_tokens(self):
        palettes = brand.parse_palettes(self.source)
        self.assertEqual(len(palettes), 6)
        self.assertEqual(palettes[0].variant, "MilkyWay")
        self.assertEqual(palettes[0].name, "Milky Way")
        self.assertEqual(palettes[0].key, "milky-way")
        self.assertEqual(
            {p.key for p in palettes},
            {
                "milky-way",
                "midnight-party",
                "confetti-sunset",
                "neon-wool",
                "alpine-pop",
                "forest-carnival",
            },
        )
        for palette in palettes:
            self.assertEqual(set(palette.colors), set(brand.COLOR_ROLES))
            for value in palette.colors.values():
                self.assertRegex(value, r"^#[0-9A-F]{6}$")
            self.assertIsInstance(palette.dark, bool)

    def test_tokens_follow_source_instead_of_a_copied_palette(self):
        original = brand.parse_palettes(self.source)[0]
        source = self.source.replace(
            f"background: 0x{original['background'][1:].lower()}",
            "background: 0x123abc",
            1,
        )
        self.assertNotEqual(source, self.source)
        changed = brand.parse_palettes(source)[0]
        self.assertEqual(changed["background"], "#123ABC")
        self.assertEqual(changed.name, original.name)
        self.assertEqual(changed.key, original.key)

    def test_empty_incomplete_and_invalid_tables_fail_closed(self):
        for source in (
            "",
            "pub enum Theme {}",
            self.source.replace("Self::MilkyWay,", "", 1),
            self.source.replace("background: 0x", "background: invalid_", 1),
            self.source.replace('=> "milky-way"', '=> "neon-wool"', 1),
            self.source.replace("dark: true,", "dark: unknown,", 1),
        ):
            with self.subTest(source=source[:50]), self.assertRaises(ValueError):
                brand.parse_palettes(source)


class BoardTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        (ROOT / "target").mkdir(exist_ok=True)
        cls.boards = brand.build_boards(ROOT)
        cls.palettes = brand.parse_palettes(
            (ROOT / "src/theme.rs").read_text(encoding="utf-8")
        )

    def test_four_boards_are_deterministic_and_current(self):
        self.assertEqual(
            set(self.boards),
            {f"felinaut-{name}-board.svg" for name in brand.BOARD_NAMES},
        )
        self.assertEqual(self.boards, brand.build_boards(ROOT))
        for name, data in self.boards.items():
            with self.subTest(board=name):
                self.assertEqual(
                    (ROOT / "references" / name).read_bytes(),
                    data,
                    "regenerate the active boards after changing their inputs",
                )
                root = ET.fromstring(data)
                self.assertEqual(root.get("viewBox"), "0 0 1600 1200")
                self.assertIn("conceptual reference", root.find(f"{SVG}title").text)
                text = " ".join(root.itertext()).lower()
                self.assertIn("not a product screenshot", text)
                self.assertNotIn("felifox", text)
                self.assertEqual(
                    {
                        node.get("data-theme")
                        for node in root.iter()
                        if node.get("data-theme")
                    },
                    {p.key for p in self.palettes},
                )

    def test_nested_canonical_assets_preserve_geometry_and_face(self):
        expected = {
            path: signature(ET.parse(ROOT / path).getroot())
            for path in (
                "assets/felinaut-mascot.svg",
                "assets/felinaut-symbol.svg",
                "assets/felinaut-box.svg",
            )
        }
        seen = set()
        for name, data in self.boards.items():
            for node in ET.fromstring(data).iter(f"{SVG}svg"):
                path = node.get("data-source")
                if path in expected and not node.get("data-motif"):
                    with self.subTest(board=name, asset=path):
                        self.assertEqual(signature(node), expected[path])
                    seen.add(path)
        self.assertEqual(seen, set(expected))

    def test_ids_resolve_locally_and_boards_have_no_external_resources(self):
        for name, data in self.boards.items():
            root = ET.fromstring(data)
            ids = [node.get("id") for node in root.iter() if node.get("id")]
            self.assertEqual(len(ids), len(set(ids)), name)
            for node in root.iter():
                self.assertNotIn(
                    node.tag,
                    {
                        f"{SVG}{tag}"
                        for tag in (
                            "image",
                            "script",
                            "foreignObject",
                            "filter",
                            "style",
                            "use",
                        )
                    },
                    name,
                )
                for attr, value in node.attrib.items():
                    self.assertFalse(attr.startswith("on"), name)
                    self.assertNotEqual(attr.rsplit("}", 1)[-1], "href", name)
                    for target in re.findall(r"url\(#([^)]*)\)", value):
                        self.assertIn(
                            target, ids, f"{name}: unresolved paint/clip {target}"
                        )

    def test_theme_swatch_values_match_every_live_rust_token(self):
        root = ET.fromstring(self.boards["felinaut-theme-board.svg"])
        actual = {
            (node.get("data-theme"), node.get("data-role")): (
                node.get("fill"),
                node.get("data-color"),
            )
            for node in root.iter()
            if node.get("data-role")
        }
        expected = {
            (p.key, role): (p[role], p[role])
            for p in self.palettes
            for role in brand.COLOR_ROLES
        }
        self.assertEqual(actual, expected)

    def test_future_personality_studies_are_labeled_not_shipped_ui(self):
        root = ET.fromstring(self.boards["felinaut-artwork-board.svg"])
        visible = [node.text or "" for node in root.iter(f"{SVG}text")]
        self.assertTrue(
            any("not implemented browser behaviors" in text for text in visible)
        )
        self.assertEqual(
            sum(text.startswith("Future ") and "concept ·" in text for text in visible),
            3,
        )
        self.assertEqual(
            sum(
                text.startswith("Current static New Tab composition ·")
                for text in visible
            ),
            1,
        )
        self.assertTrue(any("Reduce Motion" in text for text in visible))
        self.assertTrue(any("no browsing-data display" in text for text in visible))
        self.assertEqual(
            sum(
                node.get("data-source") == "assets/felinaut-mascot.svg"
                and not node.get("data-motif")
                for node in root.iter()
            ),
            4,
        )

    def test_current_new_tab_composition_layers_canonical_box_over_aligned_mascot(self):
        root = ET.fromstring(self.boards["felinaut-artwork-board.svg"])
        children = list(root)
        boxes = [
            node
            for node in children
            if node.get("data-source") == "assets/felinaut-box.svg"
        ]
        self.assertEqual(
            len(boxes), 1, "only current New Tab study 01 uses the canonical box"
        )
        box = boxes[0]
        mascots = [
            node
            for node in children
            if node.get("data-source") == "assets/felinaut-mascot.svg"
            and not node.get("data-motif")
        ]
        self.assertEqual(len(mascots), 4)
        hero = mascots[0]
        self.assertEqual(
            children.index(box),
            children.index(hero) + 1,
            "foreground box must paint after the cat",
        )
        for attr in ("x", "y", "width", "height", "viewBox"):
            self.assertEqual(
                box.get(attr), hero.get(attr), f"aligned New Tab composition: {attr}"
            )

    @unittest.skipUnless(
        shutil.which("rsvg-convert") and shutil.which("magick"),
        "canonical composition pixel validation requires local librsvg and ImageMagick",
    )
    def test_current_new_tab_raster_covers_boots_and_preserves_head_paws_and_yarn(self):
        data = self.boards["felinaut-artwork-board.svg"]
        root = ET.fromstring(data)
        hero = next(
            node
            for node in root
            if node.get("data-source") == "assets/felinaut-mascot.svg"
            and not node.get("data-motif")
        )
        bounds = tuple(float(hero.get(attr)) for attr in ("x", "y", "width", "height"))
        expected = brand.Board(
            "artwork", "Canonical composition", "Test", self.palettes, ROOT
        )
        expected.mascot(*bounds)
        expected.asset("assets/felinaut-box.svg", *bounds)

        def render(svg):
            png = subprocess.run(
                ["rsvg-convert", "--format=png"],
                input=svg,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=True,
                timeout=45,
            ).stdout
            return subprocess.run(
                ["magick", "png:-", "-depth", "8", "rgba:-"],
                input=png,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=True,
                timeout=30,
            ).stdout

        actual_pixels, expected_pixels = render(data), render(expected.bytes())
        x, y, width, height = bounds
        for sx, sy in [
            (264, 150),
            (148, 303),
            (397, 166),
            (453, 81),
            (92, 242),
            (216, 357),
            (310, 362),
            (344, 364),
            (280, 385),
        ]:
            px, py = int(x + sx * width / 560), int(y + sy * height / 420)
            index = (py * brand.WIDTH + px) * 4
            actual = actual_pixels[index : index + 4]
            expected_pixel = expected_pixels[index : index + 4]
            self.assertEqual(
                expected_pixel[3], 255, "sample an opaque canonical detail"
            )
            self.assertEqual(
                actual, expected_pixel, f"canonical New Tab detail at {sx}, {sy}"
            )
            if sy >= 350:
                self.assertTrue(
                    actual[0] > actual[1] + 20 and actual[1] > actual[2] + 20,
                    "boots must be hidden by warm brown paper, not stand beside a box",
                )

    def test_svg_only_generation_and_missing_optional_renderer(self):
        with tempfile.TemporaryDirectory(dir=ROOT / "target") as temporary:
            output = Path(temporary) / "svg-only"
            sentinel = output / "unrelated.txt"
            output.mkdir()
            sentinel.write_text("keep", encoding="utf-8")
            with patch.object(brand.shutil, "which", return_value=None):
                names = brand.generate(output, root=ROOT, png=False)
                self.assertEqual(set(names), set(self.boards))
                self.assertFalse(list(output.glob("*.png")))
                with self.assertRaisesRegex(RuntimeError, "rsvg-convert"):
                    brand.generate(output / "png", root=ROOT, png=True)
            self.assertEqual(sentinel.read_text(), "keep")
            for name, expected in self.boards.items():
                self.assertEqual((output / name).read_bytes(), expected)

    def test_missing_asset_and_missing_canonical_motif_are_actionable(self):
        b = brand.Board("brand", "Test", "Test", self.palettes, ROOT)
        with self.assertRaises(FileNotFoundError):
            b.asset("assets/not-present.svg", 0, 0, 1, 1)
        with self.assertRaisesRegex(ValueError, "missing canonical motif"):
            b.asset("assets/felinaut-mascot.svg", 0, 0, 100, 100, motif="missing-motif")

    def test_legacy_originals_are_archived_byte_for_byte(self):
        archive = ROOT / "references/archive/legacy-felifox"
        self.assertEqual({p.name for p in archive.glob("*.png")}, set(LEGACY))
        for name, digest in LEGACY.items():
            with self.subTest(name=name):
                self.assertEqual(
                    hashlib.sha256((archive / name).read_bytes()).hexdigest(), digest
                )
                self.assertFalse((ROOT / "references" / name).exists())

    @unittest.skipUnless(
        shutil.which("rsvg-convert"),
        "PNG rendering requires local librsvg/rsvg-convert",
    )
    def test_actual_png_generation_and_committed_previews(self):
        with tempfile.TemporaryDirectory(dir=ROOT / "target") as temporary:
            output = Path(temporary)
            brand.generate(output, root=ROOT)
            for name in self.boards:
                png_name = Path(name).with_suffix(".png")
                generated = (output / png_name).read_bytes()
                preview = (ROOT / "references" / png_name).read_bytes()
                for data in (generated, preview):
                    self.assertEqual(data[:8], b"\x89PNG\r\n\x1a\n")
                    self.assertEqual(
                        struct.unpack(">II", data[16:24]), (brand.WIDTH, brand.HEIGHT)
                    )
                self.assertGreater(len(generated), 10000)
                self.assertGreater(len(preview), 10000)

    @unittest.skipUnless(
        shutil.which("rsvg-convert") and shutil.which("magick"),
        "RGBA pixel validation requires local librsvg and ImageMagick",
    )
    def test_generated_and_active_pngs_have_real_artwork_pixels(self):
        with tempfile.TemporaryDirectory(dir=ROOT / "target") as temporary:
            output = Path(temporary)
            brand.generate(output, root=ROOT)
            for name in self.boards:
                png_name = Path(name).with_suffix(".png")
                for path in (output / png_name, ROOT / "references" / png_name):
                    rgba = subprocess.run(
                        ["magick", "png:-", "-depth", "8", "rgba:-"],
                        input=path.read_bytes(),
                        stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE,
                        check=True,
                        timeout=30,
                    ).stdout
                    self.assertEqual(len(rgba), brand.WIDTH * brand.HEIGHT * 4)
                    colors = {
                        rgba[index : index + 3] for index in range(0, len(rgba), 4)
                    }
                    self.assertGreater(
                        len(colors), 1000, "board must render illustration detail"
                    )
                    # Check semantic colors, not a font/renderer-version-specific golden image.
                    self.assertTrue(
                        any(
                            p[0] > 220 and 110 < p[1] < 205 and p[2] < 170
                            for p in colors
                        ),
                        "orange tabby pixels",
                    )
                    self.assertTrue(
                        any(p[1] > p[0] + 20 and p[2] > 150 for p in colors),
                        "cyan visor pixels",
                    )


if __name__ == "__main__":
    unittest.main()
