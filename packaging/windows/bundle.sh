#!/usr/bin/env bash
# Build dist/FaderFrame (a relocatable folder: bin/ with the program and
# every DLL it needs, share/ with GTK's schemas and icons, lib/ with the
# image loaders), the installer (when Inno Setup is installed) and
# dist/FaderFrame-<version>-windows-x64-portable.zip — the folder with a
# "FaderFrame Data" folder, which makes it portable. Run in an MSYS2 UCRT64 shell:
#
#   pacman -S mingw-w64-ucrt-x86_64-{gtk4,rust,pkgconf,gcc,librsvg,python,adwaita-icon-theme} zip
#   cargo build --release -p faderframe-app
#   packaging/windows/bundle.sh
#
# GLib and gdk-pixbuf find their data relative to their DLLs, so the folder
# runs from anywhere.
set -euo pipefail
shopt -s nullglob
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
prefix=${MINGW_PREFIX:-/ucrt64}
exe=${CARGO_TARGET_DIR:-target}/release/faderframe.exe
[ -f "$exe" ] || cargo build --release -p faderframe-app

out=dist/FaderFrame
rm -rf "$out"
mkdir -p "$out/bin" "$out/share/glib-2.0/schemas" "$out/share/icons" "$out/lib"
cp "$exe" "$out/bin/"
# Release builds carry debug info (for profiling): not for distribution.
strip "$out/bin/faderframe.exe"

# The DLLs a binary needs from the MSYS2 prefix (ldd lists them all).
copy_deps() {
    ldd "$1" | awk -v p="$prefix/" 'index($3, p) == 1 { print $3 }' | sort -u |
        while read -r dll; do
            [ -f "$out/bin/$(basename "$dll")" ] || cp "$dll" "$out/bin/"
        done
}
copy_deps "$out/bin/faderframe.exe"

# GTK's settings schemas and icon themes.
cp "$prefix"/share/glib-2.0/schemas/org.gtk.gtk4.*.gschema.xml "$out/share/glib-2.0/schemas/"
glib-compile-schemas "$out/share/glib-2.0/schemas"
for theme in hicolor Adwaita; do
    if [ -d "$prefix/share/icons/$theme" ]; then
        cp -r "$prefix/share/icons/$theme" "$out/share/icons/"
    fi
done

# Image loaders (SVG icons) and what they link; their cache uses paths
# relative to the installation.
if [ -d "$prefix/lib/gdk-pixbuf-2.0" ]; then
    cp -r "$prefix/lib/gdk-pixbuf-2.0" "$out/lib/"
    for loader in "$out"/lib/gdk-pixbuf-2.0/2.10.0/loaders/*.dll; do
        copy_deps "$loader"
    done
fi

cp LICENSE THIRD_PARTY_LICENSES.md "$out/"
python3 packaging/icons.py ico packaging/icons/io.github.BurningTreeC.FaderFrame.svg "$out/faderframe.ico"

# The installer first (an installed copy uses the user's profile), then
# the portable zip with its data folder.
iscc=${ISCC:-"/c/Program Files (x86)/Inno Setup 6/ISCC.exe"}
if [ -x "$iscc" ]; then
    "$iscc" -Q "-DVersion=$version" "-DSource=$(cygpath -w "$root/$out")" \
        "-DOutputDir=$(cygpath -w "$root/dist")" "$(cygpath -w packaging/windows/faderframe.iss)"
    echo "dist/FaderFrame-$version-windows-x64-setup.exe"
else
    echo "Inno Setup not found ($iscc): no installer built"
fi

mkdir -p "$out/FaderFrame Data"
cp packaging/windows/README.txt "$out/README.txt"
zip_name=FaderFrame-$version-windows-x64-portable.zip
rm -f "dist/$zip_name"
(cd dist && zip -qr "$zip_name" FaderFrame)
echo "dist/$zip_name"
