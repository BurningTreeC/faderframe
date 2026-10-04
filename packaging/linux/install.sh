#!/bin/sh
# Install FaderFrame into a prefix (default /usr/local): the binary from a
# release build, the desktop entry, AppStream metadata, the project MIME
# type and the icon.
#
#   cargo build --release -p faderframe-app
#   sudo packaging/linux/install.sh [--data-only] [PREFIX]
#
# --data-only skips the binary (the Flatpak build installs it itself).
set -eu
data_only=0
if [ "${1:-}" = "--data-only" ]; then
    data_only=1
    shift
fi
prefix=${1:-/usr/local}
root=$(cd "$(dirname "$0")/../.." && pwd)
id=io.github.BurningTreeC.FaderFrame
if [ "$data_only" = 0 ]; then
    install -Dm755 "$root/target/release/faderframe" "$prefix/bin/faderframe"
fi
install -Dm644 "$root/packaging/linux/$id.desktop" "$prefix/share/applications/$id.desktop"
install -Dm644 "$root/packaging/linux/$id.metainfo.xml" "$prefix/share/metainfo/$id.metainfo.xml"
install -Dm644 "$root/packaging/linux/$id.xml" "$prefix/share/mime/packages/$id.xml"
install -Dm644 "$root/packaging/icons/$id.svg" "$prefix/share/icons/hicolor/scalable/apps/$id.svg"
# Refresh the caches when installing system-wide (not inside a build).
if [ "$data_only" = 0 ] && [ -z "${DESTDIR:-}" ]; then
    command -v update-desktop-database >/dev/null && update-desktop-database -q "$prefix/share/applications" || true
    command -v update-mime-database >/dev/null && update-mime-database "$prefix/share/mime" || true
    command -v gtk4-update-icon-cache >/dev/null && gtk4-update-icon-cache -qtf "$prefix/share/icons/hicolor" || true
fi
