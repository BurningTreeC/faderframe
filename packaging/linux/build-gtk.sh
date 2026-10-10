#!/usr/bin/env bash
# Build the small part of the desktop stack that must be newer than Debian 13.
# The resulting libraries keep Debian 13's glibc baseline and are copied into
# the portable tarball by tarball.sh.
set -euo pipefail

prefix=${1:?usage: build-gtk.sh PREFIX}
gtk_version=4.22.4
gtk_series=${gtk_version%.*}
gtk_sha256=51bd9f60c7d23a665a556c7364c21fb2e4e282566b3e7e092455e8f910330893
wayland_version=1.24.0
wayland_sha256=82892487a01ad67b334eca83b54317a7c86a03a89cfadacfef5211f11a5d0536

export PKG_CONFIG_PATH="$prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export LD_LIBRARY_PATH="$prefix/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export PATH="$prefix/bin:$PATH"

if pkg-config --atleast-version="$gtk_version" gtk4 2>/dev/null &&
    [ "$(pkg-config --variable=prefix gtk4)" = "$prefix" ]; then
    echo "GTK $(pkg-config --modversion gtk4) is already installed in $prefix"
    exit 0
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

download() {
    local url=$1 file=$2 checksum=$3
    curl --fail --location --retry 3 --output "$file" "$url"
    printf '%s  %s\n' "$checksum" "$file" | sha256sum --check --status
}

wayland_archive=$work/wayland-$wayland_version.tar.xz
download \
    "https://gitlab.freedesktop.org/wayland/wayland/-/releases/$wayland_version/downloads/wayland-$wayland_version.tar.xz" \
    "$wayland_archive" "$wayland_sha256"
tar -C "$work" -xf "$wayland_archive"
meson setup "$work/wayland-build" "$work/wayland-$wayland_version" \
    --prefix="$prefix" --libdir=lib --buildtype=release \
    -Dtests=false -Ddocumentation=false -Ddtd_validation=false
meson compile -C "$work/wayland-build"
meson install -C "$work/wayland-build"

gtk_archive=$work/gtk-$gtk_version.tar.xz
download \
    "https://download.gnome.org/sources/gtk/$gtk_series/gtk-$gtk_version.tar.xz" \
    "$gtk_archive" "$gtk_sha256"
tar -C "$work" -xf "$gtk_archive"
meson setup "$work/gtk-build" "$work/gtk-$gtk_version" \
    --prefix="$prefix" --libdir=lib --buildtype=release \
    -Dintrospection=disabled -Ddocumentation=false -Dman-pages=false \
    -Dbuild-demos=false -Dbuild-tests=false -Dbuild-testsuite=false \
    -Dbuild-examples=false -Dprint-cups=disabled -Dmedia-gstreamer=enabled \
    -Dx11-backend=true -Dwayland-backend=true -Dvulkan=enabled
meson compile -C "$work/gtk-build"
meson install -C "$work/gtk-build"

pkg-config --atleast-version="$gtk_version" gtk4
echo "Built GTK $(pkg-config --modversion gtk4) against glibc $(getconf GNU_LIBC_VERSION)"
