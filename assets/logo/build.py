#!/usr/bin/env python3
"""Generates every icon Mach ships from the two SVG masters in this directory.

Run from the repository root: `python3 assets/logo/build.py`. It needs
`rsvg-convert`, `magick` and `iconutil`, all of which are already required to
build the app on macOS.

# Why there are two masters rather than one

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

The cut is at 32px: 16, 20 and 32 use the small master, everything above uses
the mark. Nobody perceives a cap shape three pixels across, so the seam is
invisible in the only place both appear.
"""

import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
HERE = ROOT / "assets" / "logo"
ICONS = ROOT / "src-tauri" / "icons"

# Below this, use the small master.
SMALL_THROUGH = 32


def run(*args: str) -> None:
    subprocess.run([str(a) for a in args], check=True)


def render(master: pathlib.Path, size: int, out: pathlib.Path) -> None:
    run("rsvg-convert", "-w", size, "-h", size, master, "-o", out)


def icon_master(size: int) -> pathlib.Path:
    return HERE / ("icon-small.svg" if size <= SMALL_THROUGH else "icon.svg")


def main() -> int:
    for needed in ("rsvg-convert", "magick", "iconutil"):
        if subprocess.run(["which", needed], capture_output=True).returncode:
            print(f"missing {needed}", file=sys.stderr)
            return 1

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
    # master rather than downsampled from one big one.
    ico_parts = []
    for size in (16, 32, 48, 64, 128, 256):
        part = work / f"ico-{size}.png"
        render(icon_master(size), size, part)
        ico_parts.append(part)
    run("magick", *ico_parts, ICONS / "icon.ico")
    print("  icon.ico")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
