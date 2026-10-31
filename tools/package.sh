#!/bin/sh
# Builds RubyChat's release binary and packs it with its desktop entry,
# icons and licenses into dist/rubychat-<version>-<arch>-linux.tar.gz, with
# its SHA-256 beside it (DESIGN.md §16, M7). Nothing is installed: the
# archive's install.sh does that, into a prefix of your choosing.
#
#     tools/package.sh
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
arch=$(uname -m)
name="rubychat-$version-$arch-linux"
stage="dist/$name"

cargo build --release --locked

rm -rf "$stage"
mkdir -p "$stage/bin" "$stage/share/applications" "$stage/share/doc/rubychat"
install -m 0755 target/release/rubychat "$stage/bin/rubychat"
install -m 0644 assets/desktop/rubychat.desktop "$stage/share/applications/rubychat.desktop"
install -m 0644 assets/desktop/rubychat-autostart.desktop "$stage/share/doc/rubychat/rubychat-autostart.desktop"
for icon in assets/desktop/icons/hicolor/*/apps/rubychat.png; do
    size=$(basename "$(dirname "$(dirname "$icon")")")
    mkdir -p "$stage/share/icons/hicolor/$size/apps"
    install -m 0644 "$icon" "$stage/share/icons/hicolor/$size/apps/rubychat.png"
done
install -m 0644 LICENSE.md "$stage/share/doc/rubychat/LICENSE.md"
install -m 0644 assets/fonts/JetBrainsMono-OFL.txt "$stage/share/doc/rubychat/JetBrainsMono-OFL.txt"
install -m 0644 assets/fonts/JetBrainsMono-AUTHORS.txt "$stage/share/doc/rubychat/JetBrainsMono-AUTHORS.txt"

cat > "$stage/install.sh" <<'INSTALL'
#!/bin/sh
# Installs RubyChat under PREFIX (default ~/.local): the binary, its desktop
# entry (which also opens irc:// and ircs:// links) and its icons. Run from
# the unpacked archive:
#
#     ./install.sh            # into ~/.local
#     sudo ./install.sh /usr/local
set -eu
here=$(cd "$(dirname "$0")" && pwd)
prefix=${1:-"$HOME/.local"}
mkdir -p "$prefix/bin" "$prefix/share"
install -m 0755 "$here/bin/rubychat" "$prefix/bin/rubychat"
cp -R "$here/share/." "$prefix/share/"
# Menus and icon themes notice the new files where the tools exist.
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$prefix/share/applications" || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t "$prefix/share/icons/hicolor" || true
fi
echo "RubyChat installed in $prefix/bin; its desktop entry and icons in $prefix/share."
INSTALL
chmod 0755 "$stage/install.sh"

tar -C dist -czf "dist/$name.tar.gz" "$name"
(cd dist && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "dist/$name.tar.gz"
