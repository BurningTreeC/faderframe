#!/usr/bin/env python3
"""Copy every non-system library the given Mach-O files need (recursively)
into a Frameworks directory and point all of them at the copies
(`@executable_path/../Frameworks/<name>`).

    bundle_dylibs.py FRAMEWORKS_DIR BINARY [BINARY...]

Install names are resolved like dyld does: absolute paths, `@rpath/`
(through the referencing file's LC_RPATH entries), `@loader_path/` and
`@executable_path/`. A library that cannot be found is an error (no
prompting). The binaries need header room for the longer names (link with
`-headerpad_max_install_names`; Homebrew's libraries have it).
"""

from __future__ import annotations

import functools
import os
import shutil
import subprocess
import sys

PREFIX = "@executable_path/../Frameworks/"
SYSTEM = ("/usr/lib/", "/System/")


def run(*args: str) -> str:
    return subprocess.run(args, check=True, capture_output=True, text=True).stdout


MACH_O = {0xFEEDFACE, 0xFEEDFACF, 0xCEFAEDFE, 0xCFFAEDFE, 0xCAFEBABE, 0xBEBAFECA}


def is_mach_o(path: str) -> bool:
    with open(path, "rb") as f:
        head = f.read(4)
    return len(head) == 4 and int.from_bytes(head, "big") in MACH_O


def install_names(path: str) -> list[str]:
    lines = run("otool", "-L", path).splitlines()[1:]
    return [
        line.strip().split(" (compatibility")[0].strip()
        for line in lines
        if line.strip()
    ]


@functools.cache
def own_id(path: str) -> str | None:
    lines = run("otool", "-D", path).splitlines()[1:]
    return lines[0].strip() if lines else None


def rpaths(path: str) -> list[str]:
    out, lines = [], run("otool", "-l", path).splitlines()
    for i, line in enumerate(lines):
        if line.strip() == "cmd LC_RPATH":
            for follow in lines[i + 1 : i + 4]:
                follow = follow.strip()
                if follow.startswith("path "):
                    out.append(follow[5:].split(" (offset")[0])
    return out


def resolve(name: str, origin: str, executable: str) -> str | None:
    """The file an install name refers to, seen from `origin` (the real
    location of the referencing file)."""
    loader = os.path.dirname(origin)
    exe_dir = os.path.dirname(executable)
    candidates = []
    if name.startswith("@rpath/"):
        rest = name[len("@rpath/") :]
        for rp in rpaths(origin) + rpaths(executable):
            rp = rp.replace("@loader_path", loader).replace("@executable_path", exe_dir)
            candidates.append(os.path.join(rp, rest))
        brew = os.environ.get("HOMEBREW_PREFIX", "/opt/homebrew")
        candidates += [
            os.path.join(brew, "lib", rest),
            os.path.join("/usr/local/lib", rest),
        ]
    elif name.startswith("@loader_path/"):
        candidates.append(os.path.join(loader, name[len("@loader_path/") :]))
    elif name.startswith("@executable_path/"):
        candidates.append(os.path.join(exe_dir, name[len("@executable_path/") :]))
    else:
        candidates.append(name)
    for c in candidates:
        if os.path.isfile(c):
            return os.path.realpath(c)
    return None


def main() -> None:
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    frameworks, binaries = sys.argv[1], sys.argv[2:]
    for b in binaries:
        if not is_mach_o(b):
            sys.exit(f"{b}: not a Mach-O binary")
    executable = binaries[0]
    os.makedirs(frameworks, exist_ok=True)
    # file to patch -> its real origin (for @loader_path / @rpath)
    queue = [(b, os.path.realpath(b)) for b in binaries]
    copied: dict[str, str] = {}  # real source path -> bundled name
    patched = set()
    while queue:
        path, origin = queue.pop()
        if path in patched:
            continue
        patched.add(path)
        changes = []
        for name in install_names(path):
            if name.startswith(SYSTEM) or name == own_id(path):
                continue
            source = resolve(name, origin, executable)
            if source is None:
                sys.exit(f"{path}: cannot find {name}")
            base = copied.get(source)
            if base is None:
                base = os.path.basename(source)
                target = os.path.join(frameworks, base)
                if not os.path.exists(target):
                    shutil.copy2(source, target)
                    os.chmod(target, 0o755)
                copied[source] = base
                queue.append((target, source))
            changes += ["-change", name, PREFIX + base]
        args = ["install_name_tool"]
        if os.path.dirname(os.path.abspath(path)) == os.path.abspath(frameworks):
            args += ["-id", PREFIX + os.path.basename(path)]
        if changes or len(args) > 1:
            run(*args, *changes, path)
    print(f"bundled {len(copied)} libraries into {frameworks}")


if __name__ == "__main__":
    main()
