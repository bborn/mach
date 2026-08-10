#!/usr/bin/env python3
"""Generates every icon Mach ships from the two SVG marks in this directory.

Run from anywhere: `python3 assets/logo/build.py`. It needs `rsvg-convert`,
`magick` and `iconutil`, all of which are already required to build the app on
macOS.

It writes `icon.svg` and `icon-small.svg` from `ramp.svg` and `ramp-small.svg`
plus the geometry below, then renders those to `src-tauri/icons/`. The two
`icon*.svg` are outputs, not sources — edit the ramps and the constants here.

It also writes what the published surfaces use: `mark-on-light.png` and
`mark-on-dark.png` beside this file for the README, and
`docs/social-preview.png` for GitHub's repository card and the site's
`og:image`. Those are outputs too.

# Why there are two marks rather than one

`ramp.svg` is the mark. Three lanes climbing to the right, each heavier than the
one above it, and the weight is what separates them — there is no colour doing
that job.

At 16 pixels the lanes land on 1.25, 2.25 and 3.25 pixels. The lightest greys
out against a light background, and the round caps push ink diagonally into the
gaps, which measured 1.0px at their tightest. What survives is a smudge.

So `ramp-small.svg` closes the weight spread to 3/4.5/6, widens the gaps, and
squares the caps. At 16px that reads as three lanes. It is the same mark; it is
drawn for a grid that cannot hold the original's detail, which is what an icon
set is for.

The cut is at 32px: 16 and 32 use the small mark, everything above uses the
mark.

# The margin around the body

macOS does not draw an app icon to the edge of its tile. Apple's icons put the
rounded body inside a transparent margin, and every icon in the Dock is on that
grid, so an icon drawn full-bleed reads as larger and heavier than its
neighbours at the same tile size. Mach's was full-bleed.

The fractions in BODY are measured, not quoted. Safari, Finder, Mail, Terminal
and Notes all agree, and the margin is a whole number of pixels at every size:

    tile   body    fraction   margin
      16   14x14     0.8750      1px
      32   28x28     0.8750      2px
     128  104x104    0.8125     12px
     256  206x206    0.8047     25px

The 256 figure is the canonical one — 824 of 1024 — and it is what the mark
uses. Apple stops shrinking below 128 and gives 16 and 32 a 1px and 2px margin
instead of the 1.6px and 3.1px the fraction asks for, because at those sizes the
margin comes straight out of the glyph. The small mark follows it there: 0.875,
which is both Apple's own number and the largest body that still leaves a clean
one-pixel gap.

# The glyph inside the body

The ramp is fitted so its ink spans GLYPH_OF_BODY of the body, centred. That
proportion is what makes the mark read at 16px, and it is held against the
*body*, not the canvas — so introducing the margin shrinks the glyph with it
rather than pushing it against the new edge.

"Ink" means the stroked extent, not the path: a lane's bounding box includes
half its stroke width, and for a round cap that half extends past the endpoints
as well. `_ink_bounds` computes it.
"""

import math
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
HERE = ROOT / "assets" / "logo"
ICONS = ROOT / "src-tauri" / "icons"
DOCS = ROOT / "docs"

# Below this, use the small mark.
SMALL_THROUGH = 32

# The master canvas. Everything below is in these units; 100 keeps the numbers
# in the generated SVG readable as percentages.
CANVAS = 100.0

# The rounded body as a fraction of the canvas, per mark. See the note above.
BODY = {"icon.svg": 824 / 1024, "icon-small.svg": 14 / 16}

# The ramp's ink, as a fraction of the body.
GLYPH_OF_BODY = 0.72

# Apple's squircle is a superellipse: |x|^n + |y|^n = 1 with n = 5. Sampled at
# SQUIRCLE_STEPS points, which is dense enough that the polyline is under a
# tenth of a pixel from the curve at 1024.
SQUIRCLE_N = 5
SQUIRCLE_STEPS = 240

# A hairline of white at the body's edge, so the near-black body has a lip
# against a dark wallpaper. Given for a body filling the canvas, and scaled with
# the body like everything else.
RIM_STROKE = 0.8
RIM_OPACITY = 0.14

# The body's fill, top to bottom.
BODY_TOP = "#1e1e21"
BODY_BOTTOM = "#0b0b0d"
INK = "#ffffff"

# --- what the README and the site get ----------------------------------------
#
# Neither one wants the app icon. The margin above exists because macOS draws
# every Dock icon on that grid; a README and a web page have no such grid, so
# the margin there is space around the logo that nothing uses. Both surfaces get
# the bare ramp instead, cropped to its ink.
#
# The site can inline the SVG and let the ink be `currentColor`. GitHub cannot:
# it renders a README's images inside an `<img>`, where `currentColor` has no
# author colour to resolve against and falls back to black, which disappears for
# the readers on the dark theme. So the README gets one file per background and
# picks between them with `<picture>`.

# The two inks. Neither is pure: #0b0b0d is the icon body's own black, and
# #fafafa is the site's foreground on a dark background.
DARK_INK = "#0b0b0d"
LIGHT_INK = "#fafafa"

MARK_INK = {"mark-on-light.png": DARK_INK, "mark-on-dark.png": LIGHT_INK}

# The README displays the mark at 64px. Rendering three times that keeps it
# sharp on a 3x display.
MARK_WIDTH = 192

# The image GitHub's settings page takes for the repository's social preview,
# and the `og:image` machmail.dev points at. 1280x640 is the size GitHub asks
# for and exactly the 2:1 that Slack, Twitter and iMessage crop a large card
# to, so nothing of it is trimmed.
SOCIAL = (1280, 640)
SOCIAL_MARK_HEIGHT = 180
SOCIAL_WORDMARK = "MACH"
SOCIAL_WORDMARK_SIZE = 84
# The site's wordmark is uppercase at 0.18em of tracking, which is 15 at this
# size. Helvetica's caps are 0.717em tall; the lockup is centred on the caps
# rather than on the text's em box, and that is what the figure is for.
SOCIAL_TRACKING = 15
SOCIAL_CAP_HEIGHT = 0.717
SOCIAL_FONT = "Helvetica Neue, Helvetica, Arial, sans-serif"

MASTER_HEADER = """<!-- Mach app icon. {used}
     Generated by assets/logo/build.py from {mark} — do not edit by hand.
     The body is {body:.4f} of the canvas (Apple's grid, see build.py) and the
     mark's ink is {glyph:.0%} of the body. -->
"""


def run(*args: str) -> None:
    subprocess.run([str(a) for a in args], check=True)


# --- the masters -------------------------------------------------------------


def squircle_path(centre: float, radius: float) -> str:
    """A superellipse, as an SVG path, centred on `centre` with half-width `radius`."""
    exponent = 2.0 / SQUIRCLE_N
    points = []
    for step in range(SQUIRCLE_STEPS):
        angle = 2.0 * math.pi * step / SQUIRCLE_STEPS
        cos, sin = math.cos(angle), math.sin(angle)
        x = centre + radius * math.copysign(abs(cos) ** exponent, cos)
        y = centre + radius * math.copysign(abs(sin) ** exponent, sin)
        points.append(f"{x:.2f} {y:.2f}")
    return "M" + "L".join(points) + "Z"


LANE = re.compile(
    r'<path\s+d="M\s*([\d.]+)\s+([\d.]+)\s+L\s*([\d.]+)\s+([\d.]+)"'
    r'\s+stroke-width="([\d.]+)"\s+stroke-linecap="(\w+)"\s*/>'
)


def read_mark(name: str) -> tuple[float, list[re.Match[str]], list[str]]:
    """The mark's viewBox size, its lanes, and its <path> lines verbatim."""
    text = (HERE / name).read_text()
    view = re.search(r'viewBox="0 0 ([\d.]+) [\d.]+"', text)
    if not view:
        raise SystemExit(f"{name} has no viewBox starting at 0 0")
    lanes = list(LANE.finditer(text))
    if not lanes:
        raise SystemExit(f"{name} has no lanes matching the expected <path> shape")
    return float(view.group(1)), lanes, [m.group(0) for m in lanes]


def _ink_bounds(lanes: list[re.Match[str]]) -> tuple[float, float, float, float]:
    """The bounding box of the stroked lanes, in the mark's own units.

    A stroke is the segment swept by its pen. A round cap makes that pen a disc,
    so the ink reaches half the stroke width past the segment in every
    direction. A butt cap stops at the endpoints, and the ink reaches half the
    width along the segment's normal — which is mostly vertical here, but not
    only, since the lanes climb.
    """
    xs: list[float] = []
    ys: list[float] = []
    for lane in lanes:
        x1, y1, x2, y2 = (float(lane.group(i)) for i in (1, 2, 3, 4))
        half = float(lane.group(5)) / 2.0
        round_cap = lane.group(6) == "round"
        if round_cap:
            dx = dy = half
        else:
            length = math.hypot(x2 - x1, y2 - y1)
            # The normal to the segment, scaled to the pen's half width.
            dx, dy = abs(y2 - y1) / length * half, abs(x2 - x1) / length * half
        xs += [min(x1, x2) - dx, max(x1, x2) + dx]
        ys += [min(y1, y2) - dy, max(y1, y2) + dy]
    return min(xs), min(ys), max(xs), max(ys)


def write_master(out: str, mark: str, used: str) -> None:
    body = BODY[out] * CANVAS
    centre = CANVAS / 2.0
    squircle = squircle_path(centre, body / 2.0)

    _, lanes, lines = read_mark(mark)
    left, top, right, bottom = _ink_bounds(lanes)
    # Fit the ink's longer side to its share of the body, and centre it. Both
    # sides are checked because the ramp is wider than it is tall by only a
    # little, and a taller mark must not be allowed to overhang.
    scale = GLYPH_OF_BODY * body / max(right - left, bottom - top)
    tx = centre - scale * (left + right) / 2.0
    ty = centre - scale * (top + bottom) / 2.0

    svg = MASTER_HEADER.format(used=used, mark=mark, body=BODY[out], glyph=GLYPH_OF_BODY)
    svg += (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {CANVAS:g} {CANVAS:g}"'
        f' width="1024" height="1024" fill="none">\n'
        f"  <defs>\n"
        f'    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">\n'
        f'      <stop offset="0" stop-color="{BODY_TOP}"/>\n'
        f'      <stop offset="1" stop-color="{BODY_BOTTOM}"/>\n'
        f"    </linearGradient>\n"
        f"  </defs>\n"
        f'  <path d="{squircle}" fill="url(#bg)"/>\n'
        f'  <path d="{squircle}" fill="none" stroke="{INK}" stroke-opacity="{RIM_OPACITY}"'
        f' stroke-width="{RIM_STROKE * BODY[out]:.4f}"/>\n'
        f'  <g transform="translate({tx:.3f} {ty:.3f}) scale({scale:.4f})" stroke="{INK}">\n'
    )
    for line in lines:
        svg += f"    {line}\n"
    svg += "  </g>\n</svg>\n"

    (HERE / out).write_text(svg)
    print(f"  {out}")


# --- the flat mark, and the card it goes on ----------------------------------


def ramp_ink() -> tuple[str, tuple[float, float, float, float]]:
    """The ramp's `<path>` lines verbatim, and the bounding box of their ink."""
    _, lanes, lines = read_mark("ramp.svg")
    return "\n".join(f"  {line}" for line in lines), _ink_bounds(lanes)


def flat_mark(colour: str) -> str:
    """The ramp alone, cropped to its ink, stroked in one fixed colour."""
    lines, (left, top, right, bottom) = ramp_ink()
    return (
        "<!-- Mach. Generated by assets/logo/build.py from ramp.svg — do not edit\n"
        "     by hand. The viewBox is the ink, so the mark fills the box it is\n"
        "     given and a caller can centre it by centring that box. -->\n"
        f'<svg xmlns="http://www.w3.org/2000/svg"'
        f' viewBox="{left:g} {top:g} {right - left:g} {bottom - top:g}"'
        f' fill="none" stroke="{colour}">\n{lines}\n</svg>\n'
    )


def social_preview() -> str:
    """The 1280x640 card: the mark over the wordmark, on the icon's own field.

    The card carries its own background because it is one fixed image shown
    against whatever Slack, GitHub or a phone puts behind it. The field is the
    gradient from the app icon's body, so the card and the icon in the Dock are
    the same object.
    """
    width, height = SOCIAL
    lines, (left, top, right, bottom) = ramp_ink()

    scale = SOCIAL_MARK_HEIGHT / (bottom - top)
    mark_width = (right - left) * scale

    # The lockup is the mark, a gap, then the wordmark's caps. Centring the
    # block on the caps rather than on the text's em box is what stops it
    # sitting visibly low.
    gap = 60.0
    caps = SOCIAL_CAP_HEIGHT * SOCIAL_WORDMARK_SIZE
    block_top = (height - (SOCIAL_MARK_HEIGHT + gap + caps)) / 2.0
    baseline = block_top + SOCIAL_MARK_HEIGHT + gap + caps

    tx = (width - mark_width) / 2.0 - left * scale
    ty = block_top - top * scale
    # A tracked run of text carries the tracking after its last letter too, so
    # a centred anchor lands half of it left of where it looks centred.
    text_x = width / 2.0 + SOCIAL_TRACKING / 2.0

    return (
        "<!-- Mach's social preview. Generated by assets/logo/build.py from\n"
        "     ramp.svg — do not edit by hand. -->\n"
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}"'
        f' width="{width}" height="{height}" fill="none">\n'
        f"  <defs>\n"
        f'    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">\n'
        f'      <stop offset="0" stop-color="{BODY_TOP}"/>\n'
        f'      <stop offset="1" stop-color="{BODY_BOTTOM}"/>\n'
        f"    </linearGradient>\n"
        f"  </defs>\n"
        f'  <rect width="{width}" height="{height}" fill="url(#bg)"/>\n'
        f'  <g transform="translate({tx:.3f} {ty:.3f}) scale({scale:.4f})" stroke="{INK}">\n'
        f"{lines}\n"
        f"  </g>\n"
        f'  <text x="{text_x:.1f}" y="{baseline:.1f}" text-anchor="middle"'
        f' font-family="{SOCIAL_FONT}" font-size="{SOCIAL_WORDMARK_SIZE}"'
        f' font-weight="500" letter-spacing="{SOCIAL_TRACKING}"'
        f' fill="{LIGHT_INK}">{SOCIAL_WORDMARK}</text>\n'
        f"</svg>\n"
    )


# --- the rasters -------------------------------------------------------------


def render(master: pathlib.Path, size: int, out: pathlib.Path) -> None:
    render_box(master, size, size, out)


def render_box(src: pathlib.Path, width: float, height: float, out: pathlib.Path) -> None:
    run("rsvg-convert", "-w", round(width), "-h", round(height), src, "-o", out)


def icon_master(size: int) -> pathlib.Path:
    return HERE / ("icon-small.svg" if size <= SMALL_THROUGH else "icon.svg")


def main() -> int:
    for needed in ("rsvg-convert", "magick", "iconutil"):
        if subprocess.run(["which", needed], capture_output=True).returncode:
            print(f"missing {needed}", file=sys.stderr)
            return 1

    write_master("icon.svg", "ramp.svg", "Used above 32px.")
    write_master("icon-small.svg", "ramp-small.svg", "Used at 32px and below.")

    ICONS.mkdir(parents=True, exist_ok=True)
    work = ROOT / "target-icons"
    iconset = work / "icon.iconset"
    iconset.mkdir(parents=True, exist_ok=True)

    # The five entries tauri.conf.json names, plus the 1024 source.
    for size, name in ((32, "32x32.png"), (128, "128x128.png"), (256, "128x128@2x.png"), (1024, "icon.png")):
        render(icon_master(size), size, ICONS / name)
        print(f"  {name}")

    # macOS wants both densities of every point size it draws.
    for point in (16, 32, 128, 256, 512):
        render(icon_master(point), point, iconset / f"icon_{point}x{point}.png")
        render(icon_master(point * 2), point * 2, iconset / f"icon_{point}x{point}@2x.png")
    run("iconutil", "-c", "icns", iconset, "-o", ICONS / "icon.icns")
    print("  icon.icns")

    # .ico carries its own small sizes, so each is rendered from the right
    # mark rather than downsampled from one big one.
    ico_parts = []
    for size in (16, 32, 48, 64, 128, 256):
        part = work / f"ico-{size}.png"
        render(icon_master(size), size, part)
        ico_parts.append(part)
    run("magick", *ico_parts, ICONS / "icon.ico")
    print("  icon.ico")

    # The README's mark. `_ink_bounds` gives the aspect, so the render is the
    # ink's own shape and neither axis is padded.
    _, (left, top, right, bottom) = ramp_ink()
    for name, colour in MARK_INK.items():
        source = work / f"{pathlib.Path(name).stem}.svg"
        source.write_text(flat_mark(colour))
        render_box(source, MARK_WIDTH, MARK_WIDTH * (bottom - top) / (right - left), HERE / name)
        print(f"  {name}")

    card = work / "social-preview.svg"
    card.write_text(social_preview())
    DOCS.mkdir(parents=True, exist_ok=True)
    render_box(card, *SOCIAL, DOCS / "social-preview.png")
    print("  ../docs/social-preview.png")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
