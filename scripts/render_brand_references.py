#!/usr/bin/env python3
"""Build offline Felinaut concept boards from canonical SVGs and Rust palettes.

Python 3.11+ stdlib only. PNG export additionally requires local rsvg-convert.
No browser, network, third-party Python package, or runtime dependency is used.
"""

import argparse
import copy
import re
import shutil
import subprocess
import xml.etree.ElementTree as ET
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
NS = "http://www.w3.org/2000/svg"
ET.register_namespace("", NS)
WIDTH, HEIGHT = 1600, 1200
INK, MUTED, PAPER, LINE = "#222942", "#586179", "#F6F3EF", "#D9D6E3"
FONT = "Helvetica Neue, Helvetica, Arial, sans-serif"
MONO = "Menlo, DejaVu Sans Mono, monospace"
COLOR_ROLES = (
    "background",
    "surface",
    "elevated",
    "input",
    "text",
    "muted",
    "border",
    "primary",
    "secondary",
    "accent",
    "highlight",
)
BOARD_NAMES = ("brand", "theme", "artwork", "icon")
ICONS = (
    ("speaker-muted", "Muted"),
    ("speaker-low", "Quiet"),
    ("speaker", "Speaker"),
    ("speaker-boost", "Boost"),
    ("voice", "Voice"),
    ("bass", "Bass"),
    ("natural", "Natural"),
    ("sidebar", "Sidebar"),
    ("chevron", "Disclosure"),
    ("check", "Check"),
    ("github", "GitHub"),
    ("asana", "Asana"),
)


def element(tag, parent=None, **attrs):
    node = ET.Element(
        f"{{{NS}}}{tag}",
        {key.replace("_", "-"): str(value) for key, value in attrs.items()},
    )
    if parent is not None:
        parent.append(node)
    return node


@dataclass(frozen=True)
class Palette:
    variant: str
    name: str
    key: str
    colors: dict
    dark: bool

    def __getitem__(self, role):
        return self.colors[role]


def parse_palettes(source):
    """Read the small explicit Theme tables; fail closed if their shape changes."""
    all_themes = re.search(r"pub const ALL: \[Self; (\d+)\] = \[(.*?)\];", source, re.S)
    names_match = re.search(
        r"pub fn name\(self\).*?match self \{(.*?)\n        \}", source, re.S
    )
    if not all_themes or not names_match:
        raise ValueError("missing Theme::ALL or Theme::name table")
    variants = re.findall(r"Self::(\w+)", all_themes[2])
    if (
        len(set(variants)) != 6
        or len(variants) != int(all_themes[1])
        or "MilkyWay" not in variants
    ):
        raise ValueError("expected six distinct themes including MilkyWay")
    pair_pattern = r'Self::(\w+)\s*=>\s*"([^"\n]+)"'
    names = dict(re.findall(pair_pattern, names_match[1]))
    keys = None
    for block in re.findall(
        r"fn key\(self\).*?match self \{(.*?)\n        \}", source, re.S
    ):
        candidate = dict(re.findall(pair_pattern, block))
        if set(candidate) == set(variants):
            keys = candidate
            break
    blocks = dict(
        re.findall(r"Self::(\w+) => Palette \{(.*?)\n            \}", source, re.S)
    )
    if keys is None or set(names) != set(variants) or set(blocks) != set(variants):
        raise ValueError("theme names, settings keys, and palette variants disagree")
    result = []
    for variant in ["MilkyWay", *[v for v in variants if v != "MilkyWay"]]:
        colors = {
            role: f"#{value.upper()}"
            for role, value in re.findall(
                r"(\w+):\s*0x([0-9a-fA-F]{6})\s*,", blocks[variant]
            )
        }
        dark = re.search(r"dark:\s*(true|false)\s*,", blocks[variant])
        if set(colors) != set(COLOR_ROLES) or not dark:
            raise ValueError(f"incomplete palette for {variant}")
        result.append(
            Palette(variant, names[variant], keys[variant], colors, dark[1] == "true")
        )
    if len({p.key for p in result}) != 6:
        raise ValueError("duplicate theme settings keys")
    return result


class Board:
    def __init__(self, kind, title, subtitle, palettes, root=ROOT):
        self.kind, self.palettes, self.source_root = kind, palettes, Path(root)
        self.serial = 0
        self.root = element(
            "svg", width=WIDTH, height=HEIGHT, viewBox=f"0 0 {WIDTH} {HEIGHT}"
        )
        element(
            "title", self.root
        ).text = f"Felinaut {kind} board — conceptual reference"
        element("desc", self.root).text = (
            "Original vector design reference, not an actual browser screenshot. "
            "Canonical astronaut-cat assets and all six current theme palettes. "
            "Current static compositions and future personality concepts are labeled separately."
        )
        self.rect(0, 0, WIDTH, HEIGHT, PAPER, radius=0)
        self.text(
            64,
            45,
            f"FELINAUT  /  FIELD NOTES {BOARD_NAMES.index(kind) + 1:02}",
            14,
            font=MONO,
        )
        self.text(
            1536,
            45,
            "CONCEPTUAL REFERENCE · NOT A PRODUCT SCREENSHOT",
            12,
            anchor="end",
            fill=MUTED,
        )
        self.path("M64 65H1536", stroke=LINE)
        self.text(64, 126, title, 45, weight="700")
        self.text(64, 157, subtitle, 17, fill=MUTED)

    def rect(self, x, y, width, height, fill, radius=20, parent=None, **attrs):
        return element(
            "rect",
            self.root if parent is None else parent,
            x=x,
            y=y,
            width=width,
            height=height,
            rx=radius,
            fill=fill,
            **attrs,
        )

    def text(
        self,
        x,
        y,
        text,
        size=16,
        fill=INK,
        font=FONT,
        weight="400",
        anchor="start",
        parent=None,
        **attrs,
    ):
        node = element(
            "text",
            self.root if parent is None else parent,
            x=x,
            y=y,
            fill=fill,
            font_family=font,
            font_size=size,
            font_weight=weight,
            text_anchor=anchor,
            **attrs,
        )
        node.text = text
        return node

    def lines(self, x, y, lines, size=17, step=27, fill=MUTED):
        for index, line in enumerate(lines):
            self.text(x, y + index * step, line, size, fill=fill)

    def path(self, d, fill="none", stroke=LINE, width=1.5, parent=None, **attrs):
        return element(
            "path",
            self.root if parent is None else parent,
            d=d,
            fill=fill,
            stroke=stroke,
            stroke_width=width,
            stroke_linecap="round",
            stroke_linejoin="round",
            **attrs,
        )

    def circle(self, x, y, radius, fill, parent=None, **attrs):
        return element(
            "circle",
            self.root if parent is None else parent,
            cx=x,
            cy=y,
            r=radius,
            fill=fill,
            **attrs,
        )

    def asset(self, relative, x, y, width, height, crop=None, motif=None, tint=None):
        """Inline a fresh SVG tree and namespace its IDs to avoid cross-instance paint."""
        source = ET.parse(self.source_root / relative).getroot()
        if source.tag != f"{{{NS}}}svg":
            raise ValueError(f"not an SVG: {relative}")
        if motif:
            matches = [node for node in source.iter() if node.get("id") == motif]
            if len(matches) != 1:
                raise ValueError(f"missing canonical motif {motif}")
            selected = copy.deepcopy(matches[0])
            defs = source.find(f"{{{NS}}}defs")
            source[:] = ([copy.deepcopy(defs)] if defs is not None else []) + [selected]
        self.serial += 1
        prefix = f"{self.kind}-{self.serial:03}-"
        ids = {
            node.get("id"): prefix + node.get("id")
            for node in source.iter()
            if node.get("id")
        }
        for node in source.iter():
            for name, value in list(node.attrib.items()):
                if name == "id":
                    node.set(name, ids[value])
                else:
                    value = re.sub(
                        r"url\(#([^)]*)\)", lambda m: f"url(#{ids[m[1]]})", value
                    )
                    if name.rsplit("}", 1)[-1] == "href":
                        if not value.startswith("#"):
                            raise ValueError("external SVG resource is not allowed")
                        value = "#" + ids[value[1:]]
                    if (
                        tint
                        and name in ("fill", "stroke")
                        and value in ("#000", "#000000", "currentColor")
                    ):
                        value = tint
                    node.set(name, value)
        source.set("x", str(x))
        source.set("y", str(y))
        source.set("width", str(width))
        source.set("height", str(height))
        source.set("data-source", relative)
        if crop:
            source.set("viewBox", crop)
        if motif:
            source.set("data-motif", motif)
        self.root.append(source)
        return source

    def mascot(self, x, y, width=560, height=420):
        return self.asset("assets/felinaut-mascot.svg", x, y, width, height)

    def symbol(self, x, y, size):
        return self.asset("assets/felinaut-symbol.svg", x, y, size, size)

    def yarn(self, x, y, size):
        return self.asset(
            "assets/felinaut-mascot.svg",
            x,
            y,
            size,
            size,
            crop="409 38 91 91",
            motif="yarn-planet-main",
        )

    def stars(self, x, y, width, height, palette):
        # Deterministic edge details preserve a quiet central clearing.
        for u, v, radius in [
            (0.08, 0.2, 2),
            (0.18, 0.65, 3),
            (0.9, 0.28, 3),
            (0.8, 0.75, 2),
            (0.12, 0.85, 1.5),
        ]:
            self.circle(x + u * width, y + v * height, radius, palette["highlight"])
        self.path(
            f"M{x + 20} {y + height * 0.65}Q{x + width / 2} {y + height * 1.1} {x + width - 20} {y + height * 0.4}",
            stroke=palette["primary"],
            width=1.5,
            stroke_opacity=".4",
        )

    def theme_strip(self, y=1073):
        for index, palette in enumerate(self.palettes):
            x = 64 + index * 248
            group = element("g", self.root, data_theme=palette.key)
            self.rect(
                x,
                y,
                232,
                64,
                palette["surface"],
                radius=12,
                parent=group,
                stroke=palette["border"],
                stroke_width=1,
            )
            self.circle(x + 20, y + 24, 6, palette["primary"], parent=group)
            self.text(
                x + 35,
                y + 28,
                palette.name,
                15,
                fill=palette["text"],
                weight="600",
                parent=group,
            )
            self.text(
                x + 16,
                y + 49,
                palette.key,
                11,
                fill=palette["muted"],
                font=MONO,
                parent=group,
            )

    def footer(self, strip=True):
        if strip:
            self.theme_strip()
        self.path("M64 1160H1536", stroke=LINE)
        self.text(
            64,
            1183,
            "SOURCE  BRAND.md · assets/felinaut-{mascot,symbol,box}.svg · src/theme.rs",
            11,
            fill=MUTED,
            font=MONO,
        )
        self.text(
            1536,
            1183,
            "Offline vector composition · future concepts labeled · no remote artwork",
            12,
            fill=MUTED,
            anchor="end",
        )

    def bytes(self):
        ET.indent(self.root, space="  ")
        return ET.tostring(self.root, encoding="utf-8", xml_declaration=True) + b"\n"


def brand_board(palettes, root):
    b = Board(
        "brand",
        "A little curiosity. A whole universe.",
        "A happy astronaut cat, a welcoming voice, and a soft spot for anything round and colorful.",
        palettes,
        root,
    )
    p = palettes[0]
    b.rect(64, 194, 900, 828, p["background"])
    b.stars(64, 194, 900, 650, p)
    b.text(104, 241, "THE CURIOUS EXPLORER", 13, fill=p["muted"], font=MONO)
    b.mascot(111, 258, 800, 600)
    b.text(
        111,
        914,
        "Felinaut",
        76,
        fill=p["text"],
        font="Georgia, serif",
        weight="700",
        font_style="italic",
    )
    b.text(
        114,
        963,
        "She came for the stars. She stayed for the yarn.",
        23,
        fill=p["muted"],
    )
    b.rect(988, 194, 548, 314, "#FFFFFF", stroke=LINE)
    b.text(1020, 239, "Same cat. Every orbit.", 27, weight="700")
    b.lines(
        1020,
        279,
        [
            "Warm apricot tabby · cream muzzle",
            "Visible ears inside a cyan bubble helmet",
            "White / lavender EVA suit · paw-star patch",
            "One clear smile · two coherent paws",
            "Cross-wound wool planets · trailing threads",
        ],
        18,
        37,
    )
    b.rect(988, 532, 548, 270, "#EAE5F4")
    b.symbol(1015, 566, 188)
    b.text(1232, 585, "Small but feline.", 24, weight="700")
    b.lines(
        1232,
        623,
        [
            "Optically drawn helmet",
            "64 × 64 canonical SVG",
            "16 / 24 / 32 / 64 px",
            "Not a tiny full-body scene",
        ],
        16,
        31,
    )
    b.rect(988, 826, 548, 196, "#FFFFFF", stroke=LINE)
    b.text(1020, 868, "The expedition palette", 24, weight="700")
    for index, (role, label) in enumerate(
        [
            ("primary", "Nebula"),
            ("highlight", "Visor"),
            ("accent", "Tabby"),
            ("secondary", "Aurora"),
        ]
    ):
        x = 1020 + index * 122
        b.rect(x, 889, 100, 45, p[role], radius=9)
        b.text(x, 958, label, 14)
        b.text(x, 981, p[role], 12, fill=MUTED, font=MONO)
    b.footer()
    return b


def theme_board(palettes, root):
    b = Board(
        "theme",
        "Six environments. One curious explorer.",
        "Illustrative scene studies with exact current Rust color tokens. These are concepts, not browser screenshots.",
        palettes,
        root,
    )
    for index, p in enumerate(palettes):
        x, y = 64 + (index % 3) * 501, 184 + (index // 3) * 464
        card = element("g", b.root, data_theme=p.key, data_variant=p.variant)
        b.rect(x, y, 470, 440, "#FFFFFF", parent=card, stroke=LINE)
        b.rect(x + 1, y + 1, 468, 204, p["background"], parent=card)
        b.stars(x + 1, y + 1, 468, 204, p)
        # A restrained environment gesture, with the canonical character untouched.
        if p.variant == "ConfettiSunset":
            b.circle(x + 56, y + 62, 26, p["accent"])
            b.path(
                f"M{x + 8} {y + 174}Q{x + 95} {y + 130} {x + 186} {y + 197}",
                stroke=p["primary"],
                width=2,
            )
        elif p.variant == "AlpinePop":
            b.path(
                f"M{x + 16} {y + 185}L{x + 67} {y + 101}L{x + 116} {y + 185}M{x + 48} {y + 131}L{x + 67} {y + 145}L{x + 85} {y + 131}",
                stroke=p["primary"],
                width=2,
            )
        elif p.variant == "ForestCarnival":
            for offset in (0, 26, 49):
                b.circle(
                    x + 36 + offset,
                    y + 170 - offset / 2,
                    15,
                    p["elevated"],
                    stroke=p["primary"],
                    stroke_width=1.5,
                )
        elif p.variant == "MilkyWay":
            b.path(
                f"M{x + 27} {y + 42}Q{x + 62} {y + 12} {x + 106} {y + 35}M{x + 363} {y + 178}Q{x + 410} {y + 153} {x + 447} {y + 177}",
                stroke=p["primary"],
                width=13,
                stroke_opacity=".16",
            )
        elif p.variant == "NeonWool":
            b.yarn(x + 23, y + 107, 75)
        b.mascot(x + 100, y + 7, 270, 202.5)
        b.text(x + 22, y + 242, p.name, 25, weight="700")
        b.text(x + 22, y + 268, f"{p.variant}  /  {p.key}", 12, fill=MUTED, font=MONO)
        b.text(
            x + 446,
            y + 241,
            "DEFAULT" if p.variant == "MilkyWay" else ("DARK" if p.dark else "LIGHT"),
            10,
            fill=MUTED,
            anchor="end",
            font=MONO,
        )
        for role_index, role in enumerate(COLOR_ROLES):
            sx, sy = x + 22 + (role_index % 6) * 72, y + 287 + (role_index // 6) * 73
            swatch = b.rect(
                sx,
                sy,
                59,
                22,
                p[role],
                radius=5,
                stroke=LINE,
                data_role=role,
                data_color=p[role],
                data_theme=p.key,
            )
            ET.SubElement(swatch, f"{{{NS}}}title").text = f"{p.name} {role}: {p[role]}"
            b.text(sx, sy + 37, role, 10, fill=MUTED)
            b.text(sx, sy + 52, p[role], 10, font=MONO)
    b.footer(strip=False)
    return b


def artwork_board(palettes, root):
    b = Board(
        "artwork",
        "Little habits, interstellar possibilities.",
        "Study 01: current static New Tab composition. Studies 02–04: future interactions, not implemented browser behaviors.",
        palettes,
        root,
    )
    studies = [
        (
            "01  Her little exploration box",
            [
                "A cardboard ship.",
                "Yarn planets nearby.",
                "Helmet, paws and tail",
                "stay in clear view.",
            ],
            "Current static New Tab composition · canonical foreground box",
        ),
        (
            "02  Batting things out of orbit",
            [
                "A small tab-shaped toy.",
                "A quick, gentle nudge.",
                "Never delay closing;",
                "keep controls still.",
            ],
            "Future motion concept · chrome-only · static equivalent",
        ),
        (
            "03  A warm yarn-planet rest stop",
            [
                "A quiet place to purr.",
                "A future nap study,",
                "not a signal to suspend",
                "or discard any work.",
            ],
            "Future nap concept · canonical smiling face retained",
        ),
        (
            "04  A few more discoveries",
            [
                "Round things are",
                "irresistible to her.",
                "A bounded collection;",
                "no guilt, no hard limit.",
            ],
            "Future yarn-accumulation concept · no browsing-data display",
        ),
    ]
    for index, (title, lines, note) in enumerate(studies):
        x, y = 64 + (index % 2) * 754, 184 + (index // 2) * 422
        p = palettes[[0, 1, 3, 5][index]]
        b.rect(x, y, 718, 396, p["background"], stroke=p["border"])
        b.text(x + 25, y + 44, title, 24, fill=p["text"], weight="700")
        b.lines(x + 25, y + 98, lines, 16, 27, fill=p["muted"])
        b.stars(x + 208, y + 61, 482, 270, p)
        if index == 1:
            b.rect(
                x + 644,
                y + 97,
                50,
                38,
                p["elevated"],
                radius=9,
                stroke=p["primary"],
                stroke_width=2,
            )
            b.text(x + 669, y + 122, "toy", 13, fill=p["text"], anchor="middle")
            b.path(
                f"M{x + 616} {y + 150}Q{x + 659} {y + 167} {x + 672} {y + 143}",
                stroke=p["highlight"],
                width=2,
            )
        elif index == 2:
            b.yarn(x + 390, y + 217, 160)
        elif index == 3:
            b.yarn(x + 251, y + 231, 73)
            b.yarn(x + 599, y + 259, 85)
        # Leave room below the current pose so its box is not hidden by the caption.
        bounds = (
            (x + 244, y + 45, 420, 315) if index == 0 else (x + 244, y + 60, 444, 333)
        )
        b.mascot(*bounds)
        if index == 0:
            b.asset("assets/felinaut-box.svg", *bounds)
        b.rect(x + 16, y + 352, 686, 31, p["surface"], radius=8)
        b.text(x + 28, y + 373, note, 12, fill=p["text"])
    b.text(64, 1049, "FUTURE MOTION VOCABULARY", 12, font=MONO)
    b.text(
        378,
        1049,
        "Tiny mrrp bounce · restrained tail swish · occasional loop · opt-out · Reduce Motion · pause when inactive",
        15,
        fill=MUTED,
    )
    b.footer()
    return b


def icon_board(palettes, root):
    b = Board(
        "icon",
        "A recognizable helmet. Useful controls.",
        "Canonical full-color identity at optical sizes; existing monochrome UI icons keep their functional meaning.",
        palettes,
        root,
    )
    p = palettes[0]
    b.rect(64, 184, 462, 419, "#FFFFFF", stroke=LINE)
    b.text(91, 226, "Application tile · concept", 24, weight="700")
    b.rect(170, 250, 244, 244, p["background"], radius=54)
    b.symbol(204, 279, 176)
    b.circle(199, 280, 3, p["accent"])
    b.circle(390, 461, 2, p["highlight"])
    b.lines(
        91,
        542,
        [
            "Soft deep-space tile, not a new character.",
            "An illustration study, not a shipped .icns export.",
        ],
        15,
        27,
    )
    b.rect(550, 184, 400, 419, "#EAE5F4")
    b.text(578, 226, "Keep the silhouette open", 23, weight="700")
    b.rect(
        631,
        269,
        238,
        238,
        "#F8F6FD",
        radius=0,
        stroke="#A294C3",
        stroke_dasharray="5 5",
    )
    b.symbol(650, 288, 200)
    b.lines(
        578,
        542,
        [
            "Visible ears · round visor · one smile",
            "No tiny suit or yarn scene in the symbol.",
        ],
        15,
        27,
    )
    b.rect(974, 184, 562, 419, "#FFFFFF", stroke=LINE)
    b.text(1002, 226, "Actual optical sizes", 24, weight="700")
    for row, background in enumerate(["#F6F3EF", p["background"]]):
        y = 251 + row * 129
        b.rect(1000, y, 510, 115, background, radius=12)
        for index, size in enumerate((16, 24, 32, 64)):
            cx = 1060 + index * 123
            b.symbol(cx - size / 2, y + 12 + (64 - size) / 2, size)
            b.text(
                cx,
                y + 99,
                f"{size} px",
                13,
                fill=INK if row == 0 else p["muted"],
                anchor="middle",
                font=MONO,
            )
    b.text(1002, 558, "Color artwork uses img(); masks use svg().", 16, fill=MUTED)
    b.rect(64, 627, 1472, 343, "#FFFFFF", stroke=LINE)
    b.text(
        92, 672, "Existing monochrome controls & integration marks", 25, weight="700"
    )
    b.text(
        92,
        702,
        "Canonical 24 × 24 geometry, tinted for this board. No invented downloads, private-mode or sync controls.",
        16,
        fill=MUTED,
    )
    for index, (name, label) in enumerate(ICONS):
        x, y = 130 + (index % 6) * 243, 730 + (index // 6) * 108
        b.rect(x - 15, y - 5, 174, 82, "#F6F3FC", radius=12)
        b.asset(f"assets/icons/{name}.svg", x + 54, y + 4, 32, 32, tint="#63528F")
        b.text(x + 72, y + 61, label, 14, anchor="middle")
    b.text(64, 1015, "USE THE CANONICAL FILES", 12, font=MONO)
    b.text(
        365,
        1015,
        "assets/felinaut-symbol.svg · 64 × 64 viewBox · preview 16 / 24 / 32 / 64 px · preserve the colored face",
        16,
        fill=MUTED,
    )
    b.footer()
    return b


BUILDERS = (brand_board, theme_board, artwork_board, icon_board)


def build_boards(root=ROOT):
    root = Path(root)
    palettes = parse_palettes((root / "src/theme.rs").read_text(encoding="utf-8"))
    return {
        f"felinaut-{name}-board.svg": builder(palettes, root).bytes()
        for name, builder in zip(BOARD_NAMES, BUILDERS)
    }


def generate(output, root=ROOT, png=True):
    boards = build_boards(root)
    renderer = shutil.which("rsvg-convert") if png else None
    if png and not renderer:
        raise RuntimeError(
            "PNG export requires local rsvg-convert (librsvg); use --svg-only for vectors"
        )
    output = Path(output)
    output.mkdir(parents=True, exist_ok=True)
    for name, data in boards.items():
        (output / name).write_bytes(data)
        if png:
            rendered = subprocess.run(
                [renderer, "--format=png"],
                input=data,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=True,
                timeout=45,
            ).stdout
            (output / Path(name).with_suffix(".png")).write_bytes(rendered)
    return list(boards)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=ROOT / "references")
    parser.add_argument(
        "--svg-only",
        action="store_true",
        help="do not require librsvg or write PNG previews",
    )
    args = parser.parse_args(argv)
    try:
        names = generate(args.output, png=not args.svg_only)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        parser.exit(1, f"brand reference generation failed: {error}\n")
    print(
        f"Generated {len(names)} conceptual SVG boards"
        + (" and PNG previews" if not args.svg_only else "")
        + f" in {args.output}"
    )


if __name__ == "__main__":
    main()
