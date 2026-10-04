#!/usr/bin/env bash
# Build dist/FaderFrame.app and dist/FaderFrame-<version>-macos-<arch>.dmg
# from a release build, with Homebrew's GTK 4 and its dependencies copied
# into the bundle (bundle_dylibs.py rewrites the library paths).
#
#   brew install gtk4 adwaita-icon-theme librsvg pkgconf
#   cargo build --release -p faderframe-app
#   packaging/macos/bundle.sh
#
# The app is signed ad hoc (needed to run on Apple Silicon). Without a
# Developer ID signature and notarisation, Gatekeeper asks once: open it
# with right-click → Open.
set -euo pipefail
shopt -s nullglob
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
arch=$(uname -m)
bin=${CARGO_TARGET_DIR:-target}/release/faderframe
[ -x "$bin" ] || cargo build --release -p faderframe-app
brew=$(brew --prefix)

dist=dist
app=$dist/FaderFrame.app
contents=$app/Contents
res=$contents/Resources
rm -rf "$app"
mkdir -p "$contents/MacOS" "$contents/Frameworks" "$res/share/glib-2.0/schemas" "$res/share/icons"
# The program and its launcher (the bundle's executable). Distinct names
# beyond case: macOS file systems usually ignore case.
cp "$bin" "$contents/MacOS/faderframe-bin"
install -m755 packaging/macos/launcher.sh "$contents/MacOS/FaderFrame"
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist >"$contents/Info.plist"
python3 packaging/icons.py icns packaging/icons/io.github.BurningTreeC.FaderFrame.svg "$res/FaderFrame.icns"
cp LICENSE THIRD_PARTY_LICENSES.md "$res/"

# GTK's settings schemas and icon themes.
cp "$brew"/share/glib-2.0/schemas/org.gtk.gtk4.*.gschema.xml "$res/share/glib-2.0/schemas/"
glib-compile-schemas "$res/share/glib-2.0/schemas"
for theme in hicolor Adwaita; do
    if [ -d "$brew/share/icons/$theme" ]; then
        cp -R "$brew/share/icons/$theme" "$res/share/icons/"
    fi
done

# Image loaders (SVG icons), from Homebrew's shared loader directory (it
# links every formula's loaders); the cache gets the bundle's path at
# launch.
loader_src=$brew/lib/gdk-pixbuf-2.0/2.10.0/loaders
loader_dir=$res/lib/gdk-pixbuf-2.0/2.10.0
mkdir -p "$loader_dir/loaders"
loaders=("$loader_src"/*.so)
if [ ${#loaders[@]} -gt 0 ]; then
    cp "${loaders[@]}" "$loader_dir/loaders/"
    gdk-pixbuf-query-loaders "${loaders[@]}" |
        sed -E 's|^"[^"]*/([^/"]+\.so)"|"@RESOURCES@/lib/gdk-pixbuf-2.0/2.10.0/loaders/\1"|' \
            >"$loader_dir/loaders.cache.in"
fi

# Every non-system library the binary and the loaders use.
python3 packaging/macos/bundle_dylibs.py "$contents/Frameworks" \
    "$contents/MacOS/faderframe-bin" "$loader_dir"/loaders/*.so

# Ad-hoc signatures (install_name_tool invalidated the original ones).
find "$contents/Frameworks" "$loader_dir/loaders" -type f \( -name '*.dylib' -o -name '*.so' \) \
    -exec codesign --force --sign - {} \;
codesign --force --sign - "$contents/MacOS/faderframe-bin"
codesign --force --sign - "$app"

dmg=$dist/FaderFrame-$version-macos-$arch.dmg
rm -f "$dmg"
staging=$(mktemp -d)
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
hdiutil create -volname FaderFrame -srcfolder "$staging" -ov -format UDZO "$dmg"
rm -rf "$staging"
echo "$dmg"
