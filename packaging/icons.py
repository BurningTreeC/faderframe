#!/usr/bin/env python3
"""Application icon files from the SVG: `.ico` (Windows) and `.icns`
(macOS), both holding PNG images rendered by rsvg-convert.

    packaging/icons.py ico  packaging/icons/io.github.BurningTreeC.FaderFrame.svg out.ico
    packaging/icons.py icns packaging/icons/io.github.BurningTreeC.FaderFrame.svg out.icns
"""

import struct
import subprocess
import sys


def png(svg: str, size: int) -> bytes:
    return subprocess.run(
        ["rsvg-convert", "-w", str(size), "-h", str(size), svg],
        check=True,
        capture_output=True,
    ).stdout


def ico(svg: str, out: str) -> None:
    sizes = [16, 24, 32, 48, 64, 128, 256]
    images = [png(svg, s) for s in sizes]
    offset = 6 + 16 * len(images)
    entries, data = b"", b""
    for size, image in zip(sizes, images):
        # Width/height 0 means 256.
        entries += struct.pack(
            "<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(image), offset + len(data)
        )
        data += image
    with open(out, "wb") as f:
        f.write(struct.pack("<HHH", 0, 1, len(images)) + entries + data)


def icns(svg: str, out: str) -> None:
    # PNG-capable icon types (16 px … 512 px @2x).
    types = [
        (b"icp4", 16),
        (b"icp5", 32),
        (b"icp6", 64),
        (b"ic07", 128),
        (b"ic08", 256),
        (b"ic09", 512),
        (b"ic10", 1024),
        (b"ic11", 32),
        (b"ic12", 64),
        (b"ic13", 256),
        (b"ic14", 512),
    ]
    chunks = b""
    for kind, size in types:
        image = png(svg, size)
        chunks += kind + struct.pack(">I", len(image) + 8) + image
    with open(out, "wb") as f:
        f.write(b"icns" + struct.pack(">I", len(chunks) + 8) + chunks)


def main() -> None:
    if len(sys.argv) != 4 or sys.argv[1] not in ("ico", "icns"):
        sys.exit(__doc__)
    {"ico": ico, "icns": icns}[sys.argv[1]](sys.argv[2], sys.argv[3])


if __name__ == "__main__":
    main()
