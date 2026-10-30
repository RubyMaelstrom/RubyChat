#!/usr/bin/env python3
"""Generate RubyChat's desktop icons from assets/branding/logo-256.rgba.

Usage: python3 tools/gen_icons.py

Writes assets/desktop/icons/hicolor/<size>x<size>/apps/rubychat.png at the
Icon Theme Specification's usual sizes, for rubychat.desktop's
`Icon=rubychat`. They are the logo the window and tray use, resized with
Lanczos. Needs ImageMagick's `magick`.
"""

import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "assets" / "branding" / "logo-256.rgba"
ICONS = ROOT / "assets" / "desktop" / "icons" / "hicolor"
SIZES = (16, 22, 24, 32, 48, 64, 128, 256)

for size in SIZES:
    output = ICONS / f"{size}x{size}" / "apps" / "rubychat.png"
    output.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["magick", "-size", "256x256", "-depth", "8", f"rgba:{SOURCE}",
         "-filter", "Lanczos", "-resize", f"{size}x{size}", "-strip",
         f"png32:{output}"],
        check=True)
    print(output.relative_to(ROOT), output.stat().st_size, "bytes")
