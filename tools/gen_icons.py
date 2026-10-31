#!/usr/bin/env python3
"""Generate RubyChat's desktop icons from assets/branding/logo-256.rgba.

Usage: python3 tools/gen_icons.py

Writes assets/desktop/icons/hicolor/<size>x<size>/apps/rubychat.png at the
Icon Theme Specification's usual sizes, for rubychat.desktop's
`Icon=rubychat`. They are the logo the window and tray use, resized with
Lanczos. Then assets/desktop/rubychat.ico, which build.rs puts in
rubychat.exe: 32-bit BMP images up to 64 px, as ImageMagick writes them,
and the 128 and 256 px PNGs as they are, which Windows reads since Vista.
Needs ImageMagick's `magick`.
"""

import struct
import subprocess
import tempfile
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

# The Windows icon: BMP entries from ImageMagick's ICO, PNG for the large.
ICO = ROOT / "assets" / "desktop" / "rubychat.ico"
BMP_SIZES = (16, 24, 32, 48, 64)
PNG_SIZES = (128, 256)


def png(size):
    return ICONS / f"{size}x{size}" / "apps" / "rubychat.png"


with tempfile.TemporaryDirectory() as scratch:
    small = Path(scratch) / "small.ico"
    subprocess.run(["magick", *[str(png(size)) for size in BMP_SIZES], str(small)], check=True)
    data = small.read_bytes()
    _, kind, count = struct.unpack_from("<HHH", data, 0)
    assert kind == 1 and count == len(BMP_SIZES)
    images = []
    for n in range(count):
        width, height, colors, _, planes, bits, length, offset = struct.unpack_from(
            "<BBBBHHII", data, 6 + 16 * n)
        images.append((width, height, colors, planes, bits, data[offset:offset + length]))
for size in PNG_SIZES:
    side = 0 if size == 256 else size
    images.append((side, side, 0, 1, 32, png(size).read_bytes()))
header = struct.pack("<HHH", 0, 1, len(images))
offset = 6 + 16 * len(images)
entries, blobs = b"", b""
for width, height, colors, planes, bits, blob in images:
    entries += struct.pack("<BBBBHHII", width, height, colors, 0, planes, bits, len(blob), offset)
    blobs += blob
    offset += len(blob)
ICO.write_bytes(header + entries + blobs)
print(ICO.relative_to(ROOT), ICO.stat().st_size, "bytes")
