#!/bin/sh
# FaderFrame.app's executable: points GTK at the data inside the bundle,
# then starts the real binary.
here=$(cd "$(dirname "$0")" && pwd)
res="$here/../Resources"
export XDG_DATA_DIRS="$res/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
export GSETTINGS_SCHEMA_DIR="$res/share/glib-2.0/schemas"
loaders="$res/lib/gdk-pixbuf-2.0/2.10.0"
if [ -f "$loaders/loaders.cache.in" ]; then
    cache="${TMPDIR:-/tmp}/faderframe-pixbuf-loaders.cache"
    sed "s|@RESOURCES@|$res|g" "$loaders/loaders.cache.in" >"$cache" &&
        export GDK_PIXBUF_MODULE_FILE="$cache"
fi
# Finder used to pass a process serial number; it is no option of ours.
for arg in "$@"; do
    shift
    case "$arg" in
    -psn_*) ;;
    *) set -- "$@" "$arg" ;;
    esac
done
exec "$here/faderframe" "$@"
