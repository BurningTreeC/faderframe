#!/usr/bin/env python3
"""Look up the frames of a Windows crash report in a release's symbols.

FaderFrame's Windows program is stripped; its crash reports name every
frame as module + offset (faderframe-crash). The release publishes the
program's debug info beside the installer
(FaderFrame-<version>-windows-x64-symbols.zip, made by
packaging/windows/bundle.sh). This prints the report with each of the
program's frames followed by the functions and source lines it is in
(inlined ones too):

    scripts/symbolize_crash.py crash-....log FaderFrame-0.47.0-windows-x64-symbols.zip

The symbols may be the zip, the faderframe.exe.debug in it, or an
unstripped faderframe.exe. The report's build id (the module's "pdb" id)
is checked against the symbols' own: other symbols would name wrong
lines. Needs an addr2line that reads PE files: x86_64-w64-mingw32-addr2line,
llvm-addr2line or MSYS2's addr2line ($ADDR2LINE chooses).

Only the Python standard library is used.
"""

import argparse
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import zipfile


def pe_info(path):
    """The PE file's ImageBase and CodeView id (the build id), or None."""
    with open(path, "rb") as f:
        data = f.read()
    if data[:2] != b"MZ":
        raise SystemExit(f"{path}: not a PE file")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe : pe + 4] != b"PE\0\0":
        raise SystemExit(f"{path}: not a PE file")
    sections, optional_size = struct.unpack_from("<H12xH", data, pe + 6)
    opt = pe + 24
    magic = struct.unpack_from("<H", data, opt)[0]
    if magic == 0x20B:
        image_base = struct.unpack_from("<Q", data, opt + 24)[0]
        dirs = opt + 112
    elif magic == 0x10B:
        image_base = struct.unpack_from("<I", data, opt + 28)[0]
        dirs = opt + 96
    else:
        raise SystemExit(f"{path}: unknown optional header")
    table = opt + optional_size
    spans = []
    for i in range(sections):
        s = table + 40 * i
        vsize, vaddr, rsize, raw = struct.unpack_from("<IIII", data, s + 8)
        spans.append((vaddr, max(vsize, rsize), raw))

    def offset_of(rva):
        for vaddr, size, raw in spans:
            if vaddr <= rva < vaddr + size:
                return raw + rva - vaddr
        return None

    build_id = None
    debug_rva, debug_size = struct.unpack_from("<II", data, dirs + 8 * 6)
    at = offset_of(debug_rva) if debug_rva else None
    if at is not None:
        for e in range(min(debug_size // 28, 16)):
            kind, _size, _rva, raw = struct.unpack_from("<12xIIII", data, at + 28 * e)
            if kind == 2 and data[raw : raw + 4] == b"RSDS":
                guid = data[raw + 4 : raw + 20]
                age = struct.unpack_from("<I", data, raw + 20)[0]
                d1, d2, d3 = struct.unpack_from("<IHH", guid)
                build_id = f"{d1:08X}{d2:04X}{d3:04X}{guid[8:].hex().upper()}{age:X}"
                break
    return image_base, build_id


def addr2line():
    wanted = os.environ.get("ADDR2LINE")
    for tool in [wanted] if wanted else ["x86_64-w64-mingw32-addr2line", "llvm-addr2line", "addr2line"]:
        if tool and shutil.which(tool):
            return tool
    raise SystemExit("no addr2line found (set ADDR2LINE)")


def tidy_path(path):
    """Source paths without the build machine's folders."""
    path = path.replace("\\", "/")
    path = re.sub(r"^/rustc/[0-9a-f]+/", "", path)
    for marker in ("/rustlib/src/rust/", "/.cargo/registry/src/", "/.cargo/git/checkouts/"):
        if marker in path:
            rest = path.split(marker, 1)[1]
            return rest.split("/", 1)[1] if marker != "/rustlib/src/rust/" else rest
    if "/crates/" in path:
        return "crates/" + path.rsplit("/crates/", 1)[1]
    return path


def tidy_function(name):
    return re.sub(r"::h[0-9a-f]{16}$", "", name)


def look_up(tool, symbols, addresses):
    """address -> [(function, place)], innermost first."""
    if not addresses:
        return {}
    out = subprocess.run(
        [tool, "-e", symbols, "-f", "-i", "-C", "-a"] + [hex(a) for a in addresses],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.splitlines()
    found, current, i = {}, None, 0
    while i < len(out):
        line = out[i].strip()
        if re.fullmatch(r"0x[0-9a-fA-F]+", line):
            current = int(line, 16)
            found[current] = []
            i += 1
            continue
        if current is not None and i + 1 < len(out):
            found[current].append((tidy_function(line), tidy_path(out[i + 1].strip())))
        i += 2
    return found


FRAME = re.compile(r"^\s+(\d+)\s+0x[0-9a-f]+\s+(\S+)\+0x([0-9a-f]+)")
MODULE = re.compile(r"^\s+0x[0-9a-f]+ 0x[0-9a-f]+ (\S+)")
BUILD_ID = re.compile(r" pdb (?:\S+ )?([0-9A-F]{33,40})(?=\s)")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("report", help="the crash report (crash-*.log)")
    parser.add_argument("symbols", help="the symbols zip, faderframe.exe.debug or an unstripped faderframe.exe")
    parser.add_argument("--module", help="the module the symbols are of (default: from their file name)")
    args = parser.parse_args()

    with tempfile.TemporaryDirectory() as tmp:
        symbols = args.symbols
        if zipfile.is_zipfile(symbols):
            with zipfile.ZipFile(symbols) as z:
                members = [n for n in z.namelist() if n.endswith((".debug", ".exe"))]
                if not members:
                    raise SystemExit(f"{symbols}: no symbols in it")
                symbols = z.extract(members[0], tmp)
        module = (args.module or os.path.basename(symbols).removesuffix(".debug")).lower()
        image_base, build_id = pe_info(symbols)

        with open(args.report, encoding="utf-8", errors="replace") as f:
            lines = f.read().splitlines()
        reported, listed = None, False
        for line in lines:
            m = MODULE.match(line)
            if m and m.group(1).lower() == module:
                b = BUILD_ID.search(line)
                reported, listed = (b.group(1) if b else None), True
        if not listed:
            raise SystemExit(f"the report lists no module {module} (name the one the symbols are of with --module)")
        if reported and build_id and reported != build_id:
            print(
                f"!! the report's {module} has build id {reported}, these symbols {build_id}:\n"
                "!! they are of another build, and the lines below would be wrong.",
                file=sys.stderr,
            )
            sys.exit(1)
        if not reported or not build_id:
            print(f"(no build id to check the symbols against: the report's {reported}, the symbols' {build_id})", file=sys.stderr)

        wanted = {}
        for n, line in enumerate(lines):
            m = FRAME.match(line)
            if m and m.group(2).lower() == module:
                # After the first, a frame's address is the one its call
                # returns to: the call is the byte before.
                back = 1 if int(m.group(1)) > 0 else 0
                wanted[n] = image_base + int(m.group(3), 16) - back
        found = look_up(addr2line(), symbols, sorted(set(wanted.values())))
        for n, line in enumerate(lines):
            print(line)
            for depth, (function, place) in enumerate(found.get(wanted.get(n), [])):
                if function == "??" and place.startswith("??"):
                    continue
                how = "in" if depth == 0 else "inlined into"
                print(f"{'':12}{how} {function} at {place}")


if __name__ == "__main__":
    main()
