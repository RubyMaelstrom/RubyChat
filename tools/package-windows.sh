#!/bin/sh
# Cross-builds RubyChat for Windows x64 and packs it as TRust's releases
# are (DESIGN.md §17, 39 and 40): dist/rubychat-windows-x86-64.zip, with
# rubychat.exe and the licenses at its root, and its SHA-256 beside it.
# Needs the x86_64-pc-windows-msvc Rust target, cargo-xwin, Clang and LLD,
# and Python 3 for the zip.
#
#     tools/package-windows.sh
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
target=x86_64-pc-windows-msvc
name=rubychat-windows-x86-64
stage="dist/$name"

cargo xwin build --release --locked --target "$target"

rm -rf "$stage" "dist/$name.zip"
mkdir -p "$stage"
cp "target/$target/release/rubychat.exe" "$stage/rubychat.exe"
cp LICENSE.md assets/fonts/JetBrainsMono-OFL.txt assets/fonts/JetBrainsMono-AUTHORS.txt "$stage/"

python3 -I - "$stage" "dist/$name.zip" <<'ZIP'
import pathlib
import sys
import zipfile

stage, archive = pathlib.Path(sys.argv[1]), sys.argv[2]
with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as out:
    for path in sorted(stage.iterdir()):
        out.write(path, path.name)
ZIP
(cd dist && sha256sum "$name.zip" > "$name.zip.sha256")
echo "dist/$name.zip"
