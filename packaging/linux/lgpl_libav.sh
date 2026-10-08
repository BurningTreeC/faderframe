#!/bin/sh
# Build an LGPL FFmpeg with the decoders post-production video needs and
# GStreamer's libav plugin against it, into PREFIX:
#
#     packaging/linux/lgpl_libav.sh PREFIX
#
# A distribution's FFmpeg is usually built with GPL parts (x264, x265…),
# which FaderFrame's packages must not carry; this one has decoders only
# (ProRes, DNxHD/DNxHR, MPEG-2, DV, HuffYUV, FFV1, Cineform, JPEG 2000, …)
# and no GPL or non-free code (FFmpeg's configure refuses those unless
# asked). tarball.sh takes the result with FADERFRAME_LIBAV=PREFIX.
#
# Environment: FFMPEG_VERSION (default 7.1.2), GST_VERSION (default: the
# installed GStreamer's — the plugin must match it), WORK (a build folder;
# default: a temporary one), JOBS (default: nproc).
set -eu

prefix=$(realpath -m "$1")
ffmpeg_version=${FFMPEG_VERSION:-7.1.2}
gst_version=${GST_VERSION:-$(pkg-config --modversion gstreamer-1.0)}
work=${WORK:-$(mktemp -d)}
jobs=${JOBS:-$(nproc)}
mkdir -p "$work" "$prefix"

fetch() {
    if [ ! -d "$work/$2" ]; then
        curl -fsSL "$1" | tar xJ -C "$work"
    fi
}

fetch "https://ffmpeg.org/releases/ffmpeg-$ffmpeg_version.tar.xz" "ffmpeg-$ffmpeg_version"
fetch "https://gstreamer.freedesktop.org/src/gst-libav/gst-libav-$gst_version.tar.xz" \
    "gst-libav-$gst_version"

decoders=prores,dnxhd,mpeg2video,mpeg1video,dvvideo,huffyuv,ffvhuff,ffv1,cfhd,jpeg2000,v210,v410,r210,r10k,qtrle,rawvideo,mjpeg,utvideo,hqx,hq_hqa,pcm_s16le,pcm_s24le,pcm_s32le,pcm_f32le
parsers=dnxhd,mpegvideo,dvd_nav,mjpeg,jpeg2000
asm=""
command -v nasm >/dev/null 2>&1 || asm=--disable-x86asm

cd "$work/ffmpeg-$ffmpeg_version"
if [ ! -f "$prefix/lib/pkgconfig/libavcodec.pc" ]; then
    # shellcheck disable=SC2086 # $asm is one flag or none
    ./configure --prefix="$prefix" --libdir="$prefix/lib" \
        --enable-shared --disable-static --disable-programs --disable-doc \
        --disable-network --disable-autodetect --disable-everything \
        --disable-avdevice --disable-swresample --disable-postproc \
        --enable-avcodec --enable-avformat --enable-avfilter --enable-swscale \
        --enable-decoder="$decoders" --enable-parser="$parsers" \
        --enable-filter=yadif,scale,format,null,buffer,buffersink \
        $asm
    # The licence the build ended up with: LGPL, or stop.
    if ! grep -q '"LGPL version 2.1 or later"' config.h; then
        echo "FFmpeg was not configured as LGPL-2.1-or-later" >&2
        exit 1
    fi
    make -j"$jobs"
    make install
fi

cd "$work/gst-libav-$gst_version"
PKG_CONFIG_PATH="$prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}" \
    meson setup build --prefix="$prefix" --libdir=lib -Ddoc=disabled --wipe 2>/dev/null ||
    PKG_CONFIG_PATH="$prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}" \
        meson setup build --prefix="$prefix" --libdir=lib -Ddoc=disabled
ninja -C build -j"$jobs"
ninja -C build install
echo "LGPL libav in $prefix: $(find "$prefix/lib" -name 'libgstlibav*' | head -1)"
