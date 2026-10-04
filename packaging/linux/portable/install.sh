#!/bin/sh
# Install the FaderFrame in this folder.
#
#   ./install.sh               for you: ~/.local/opt/faderframe, the command
#                              `faderframe` in ~/.local/bin, a menu entry
#   sudo ./install.sh --system for everyone: /opt/faderframe, /usr/local/bin,
#                              /usr/local/share
#   ./install.sh --uninstall   (with --system for a system-wide one) removes it
#
# Installed copies keep their settings in your profile; this folder stays
# portable (run ./faderframe) whether or not you install it.
set -eu
here=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)
id=io.github.BurningTreeC.FaderFrame
system=0
uninstall=0
for arg in "$@"; do
    case "$arg" in
    --system) system=1 ;;
    --uninstall) uninstall=1 ;;
    -h | --help)
        sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
        exit 0
        ;;
    *)
        echo "unknown option: $arg (see --help)" >&2
        exit 2
        ;;
    esac
done

if [ "$system" = 1 ]; then
    if [ "$(id -u)" != 0 ]; then
        echo "--system needs root: sudo $0 $*" >&2
        exit 1
    fi
    prefix=/opt/faderframe
    bindir=/usr/local/bin
    share=/usr/local/share
else
    prefix="$HOME/.local/opt/faderframe"
    bindir="$HOME/.local/bin"
    share="${XDG_DATA_HOME:-$HOME/.local/share}"
fi

refresh() {
    command -v update-desktop-database >/dev/null && update-desktop-database -q "$share/applications" 2>/dev/null || true
    command -v update-mime-database >/dev/null && update-mime-database "$share/mime" 2>/dev/null || true
    command -v gtk4-update-icon-cache >/dev/null && gtk4-update-icon-cache -qtf "$share/icons/hicolor" 2>/dev/null || true
}

if [ "$uninstall" = 1 ]; then
    rm -rf "$prefix"
    rm -f "$bindir/faderframe" \
        "$share/applications/$id.desktop" \
        "$share/metainfo/$id.metainfo.xml" \
        "$share/mime/packages/$id.xml" \
        "$share/icons/hicolor/scalable/apps/$id.svg"
    refresh
    echo "FaderFrame removed from $prefix"
    exit 0
fi

if [ "$here" = "$prefix" ]; then
    echo "already installed here" >&2
    exit 1
fi
rm -rf "$prefix"
mkdir -p "$prefix" "$bindir"
cp -a "$here/." "$prefix/"
# An installed copy uses your profile, not a data folder of its own.
rm -rf "$prefix/FaderFrame Data"
ln -sf "$prefix/faderframe" "$bindir/faderframe"
install -Dm644 "$here/share/applications/$id.desktop" "$share/applications/$id.desktop"
sed -i "s|^Exec=.*|Exec=\"$prefix/faderframe\" %f|" "$share/applications/$id.desktop"
install -Dm644 "$here/share/metainfo/$id.metainfo.xml" "$share/metainfo/$id.metainfo.xml"
install -Dm644 "$here/share/mime/packages/$id.xml" "$share/mime/packages/$id.xml"
install -Dm644 "$here/share/icons/hicolor/scalable/apps/$id.svg" \
    "$share/icons/hicolor/scalable/apps/$id.svg"
refresh
echo "FaderFrame installed in $prefix; run it from your menu or with: faderframe"
case ":$PATH:" in
*":$bindir:"*) ;;
*) echo "(add $bindir to your PATH to run \`faderframe\` from a terminal)" ;;
esac
