#!/usr/bin/env bash
# Build the portable Linux tarball dist/faderframe-<version>-linux-<arch>.tar.xz
# from a release build:
#
#   faderframe-<version>/
#     faderframe              runs FaderFrame from this folder (no installation)
#     install.sh              installs it for you, or for everyone (--system);
#                             --uninstall removes it again
#     bin/faderframe          the program
#     lib/                    GTK 4 and the libraries it needs, image loaders
#     share/                  GTK's settings schemas and icon themes, the
#                             desktop entry, AppStream data, MIME type, icon
#     FaderFrame Data/        portable mode: settings, caches, presets and
#                             recordings of unsaved projects stay in here
#
# The system provides the C library, graphics drivers, the display server
# and audio server client libraries and fonts; everything else GTK needs is
# bundled, so the tarball runs on distributions at least as new as the one
# it was built on (CI: Ubuntu 24.04), whatever GTK they have.
#
#   cargo build --release -p faderframe-app
#   packaging/linux/tarball.sh            (FADERFRAME_BIN=<path> for another build)
set -euo pipefail
shopt -s nullglob
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
arch=$(uname -m)
bin=${FADERFRAME_BIN:-${CARGO_TARGET_DIR:-target}/release/faderframe}
[ -x "$bin" ] || cargo build --release -p faderframe-app
id=io.github.BurningTreeC.FaderFrame
name=faderframe-$version-linux-$arch
out=dist/$name
rm -rf "$out"
mkdir -p "$out/bin" "$out/lib" "$out/share/glib-2.0/schemas" "$out/share/icons"
cp "$bin" "$out/bin/faderframe"
strip --strip-debug "$out/bin/faderframe" 2>/dev/null || true

# Libraries the system must provide (C runtime, graphics, display and audio
# servers, D-Bus/udev, fonts, compression).
# (Names ending in "\." are exact; the others are families.)
system='^(ld-linux|linux-vdso|libc\.|libm\.|libdl\.|libpthread\.|librt\.|libresolv\.|libutil\.|libgcc_s\.|libstdc\+\+\.|libGL|libEGL|libOpenGL|libgbm\.|libdrm|libvulkan\.|libwayland-|libX|libxcb|libxkbcommon|libasound\.|libjack|libpipewire|libpulse|libdbus|libsystemd\.|libudev\.|libcap\.|libfontconfig\.|libfreetype\.|libexpat\.|libz\.|libbz2\.|liblzma\.|libzstd\.|libgcrypt\.|libgpg-error\.)'
copy_deps() {
    ldd "$1" | awk '$3 ~ /^\// { print $3 }' | sort -u | while read -r lib; do
        base=$(basename "$lib")
        if [[ $base =~ $system ]] || [ -e "$out/lib/$base" ]; then
            continue
        fi
        cp -L "$lib" "$out/lib/$base"
    done
}
copy_deps "$out/bin/faderframe"

# Image loaders (SVG icons) and what they need; the launcher writes their
# cache with this folder's path.
pixbuf_dir=$(pkg-config --variable=gdk_pixbuf_moduledir gdk-pixbuf-2.0)
loaders=$out/lib/gdk-pixbuf-2.0/2.10.0
mkdir -p "$loaders/loaders"
for so in "$pixbuf_dir"/*.so; do
    cp -L "$so" "$loaders/loaders/"
    copy_deps "$so"
done
# On PATH (Arch, Fedora) or beside the loaders (Debian, Ubuntu).
query=""
for candidate in "$(command -v gdk-pixbuf-query-loaders || true)" \
    "$(pkg-config --variable=gdk_pixbuf_query_loaders gdk-pixbuf-2.0 || true)" \
    "$(dirname "$(dirname "$pixbuf_dir")")/gdk-pixbuf-query-loaders"; do
    if [ -n "$candidate" ] && [ -x "$candidate" ]; then
        query=$candidate
        break
    fi
done
if [ -z "$query" ]; then
    echo "gdk-pixbuf-query-loaders not found" >&2
    exit 1
fi
"$query" "$loaders"/loaders/*.so |
    sed "s|$root/$out|@ROOT@|g" >"$loaders/loaders.cache.in"
# No GIO modules from the system (built against its own GLib).
mkdir -p "$out/lib/gio/modules"

# GTK's settings schemas and icon themes.
schemas=$(pkg-config --variable=prefix gtk4)/share/glib-2.0/schemas
cp "$schemas"/org.gtk.gtk4.*.gschema.xml "$out/share/glib-2.0/schemas/"
glib-compile-schemas "$out/share/glib-2.0/schemas"
for theme in hicolor Adwaita; do
    for dir in /usr/share/icons/$theme; do
        cp -r "$dir" "$out/share/icons/"
    done
done

# Desktop integration (used by install.sh) and the icon.
install -Dm644 "packaging/linux/$id.desktop" "$out/share/applications/$id.desktop"
install -Dm644 "packaging/linux/$id.metainfo.xml" "$out/share/metainfo/$id.metainfo.xml"
install -Dm644 "packaging/linux/$id.xml" "$out/share/mime/packages/$id.xml"
install -Dm644 "packaging/icons/$id.svg" "$out/share/icons/hicolor/scalable/apps/$id.svg"

install -m755 packaging/linux/portable/faderframe "$out/faderframe"
install -m755 packaging/linux/portable/install.sh "$out/install.sh"
install -m644 packaging/linux/portable/README.txt "$out/README.txt"
cp LICENSE THIRD_PARTY_LICENSES.md "$out/"
mkdir -p "$out/FaderFrame Data"

tarball=dist/$name.tar.xz
rm -f "$tarball"
tar -C dist -cJf "$tarball" "$name"
echo "$tarball"
