#!/usr/bin/env python3
"""Generate assets/branding/logo-256.rgba from assets/branding/logo.png.

Usage: python3 tools/gen_logo.py

The source is the chosen Ideogram rendering (a magenta ruby speech bubble with
cyan edges on dark navy). This crops it around the gem, keys the background to
transparency, removes the background's tint from antialiased edges, and writes
256x256 straight (non-premultiplied) RGBA. Needs ImageMagick's `magick`.
"""

import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "assets" / "branding" / "logo.png"
OUTPUT = ROOT / "assets" / "branding" / "logo-256.rgba"
SIZE = 256
# The gem's bounds in the 1024px source (ImageMagick -fuzz 6% -trim), plus room.
CROP = "420x420+302+307"
# The backdrop's color, sampled from the source's corners.
BACKGROUND = (8, 5, 37)
# Below this channel distance from the backdrop a pixel is transparent; at
# SOLID it is opaque.
CLEAR, SOLID = 10.0, 70.0

rgb = subprocess.run(
    ["magick", str(SOURCE), "-crop", CROP, "+repage", "-filter", "Lanczos",
     "-resize", f"{SIZE}x{SIZE}!", "-depth", "8", "rgb:-"],
    check=True, capture_output=True).stdout
assert len(rgb) == SIZE * SIZE * 3, len(rgb)

out = bytearray()
for i in range(SIZE * SIZE):
    r, g, b = rgb[3 * i:3 * i + 3]
    distance = max(abs(r - BACKGROUND[0]), abs(g - BACKGROUND[1]), abs(b - BACKGROUND[2]))
    alpha = min(max((distance - CLEAR) / (SOLID - CLEAR), 0.0), 1.0)
    if alpha <= 0.0:
        out += bytes((0, 0, 0, 0))
        continue
    # pixel = alpha * color + (1 - alpha) * backdrop: solve for color.
    color = [min(max(round((c - (1.0 - alpha) * k) / alpha), 0), 255)
             for c, k in zip((r, g, b), BACKGROUND)]
    out += bytes(color + [round(alpha * 255)])
OUTPUT.write_bytes(bytes(out))
print("wrote", OUTPUT, len(out), "bytes")
