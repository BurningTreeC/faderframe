#!/usr/bin/env python3
"""CHANGELOG.md from the git history: one section per release tag.

Each release lists the commits since the tag before it (the "Version x.y.z"
bump itself left out), newest release first: a commit's subject is the
entry, its message the details under it (folded), trailers left out. Issue
and pull request numbers link to GitHub.

    python3 scripts/changelog.py                  # writes CHANGELOG.md
    python3 scripts/changelog.py --check          # fails if it is stale
    python3 scripts/changelog.py --release v0.46.0  # one release's notes (stdout)

The release workflow uses --release for the GitHub release's notes; run the
script after tagging a release to bring CHANGELOG.md up to date.
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

REPO = "https://github.com/BurningTreeC/faderframe"
ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "CHANGELOG.md"

TRAILER = re.compile(r"^(Co-Authored-By|Claude-Session|Signed-off-by|Generated with)\b", re.I)
BUMP = re.compile(r"^Version \d+\.\d+\.\d+")
ISSUE = re.compile(r"(?<![\w/&\[])#(\d+)\b")


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=ROOT, check=True, capture_output=True, text=True
    ).stdout


def tags() -> list[str]:
    """Release tags, oldest first."""
    found = git("tag", "--list", "v*", "--sort=v:refname").split()
    return [t for t in found if re.fullmatch(r"v\d+\.\d+\.\d+", t)]


def commits(rng: str) -> list[tuple[str, str]]:
    """(subject, body) of the commits in `rng`, newest first."""
    out = git("log", "--no-merges", "--format=%x1e%s%x1f%b", rng)
    result = []
    for entry in out.split("\x1e")[1:]:
        subject, _, body = entry.partition("\x1f")
        subject = subject.strip()
        if BUMP.match(subject):
            continue
        lines = [l for l in body.strip().splitlines() if not TRAILER.match(l.strip())]
        result.append((subject, "\n".join(lines).strip()))
    return result


def link(text: str) -> str:
    return ISSUE.sub(lambda m: f"[#{m[1]}]({REPO}/issues/{m[1]})", text)


def release(tag: str, previous: str | None) -> str:
    """One release's notes (without its heading)."""
    rng = f"{previous}..{tag}" if previous else tag
    parts = []
    for subject, body in commits(rng):
        entry = f"- **{link(subject)}**"
        if body:
            entry += (
                "\n\n  <details><summary>Details</summary>\n\n"
                + "\n".join(f"  {l}" if l else "" for l in link(body).splitlines())
                + "\n\n  </details>"
            )
        parts.append(entry)
    if previous:
        parts.append(f"[All changes since {previous[1:]}]({REPO}/compare/{previous}...{tag})")
    return "\n\n".join(parts) + "\n"


def changelog() -> str:
    all_tags = tags()
    out = [
        "# Changelog\n",
        "What changed in each FaderFrame release, newest first. Generated from "
        "the commit history by `scripts/changelog.py`; the downloads are on "
        f"the [releases page]({REPO}/releases).\n",
    ]
    for i in range(len(all_tags) - 1, -1, -1):
        tag = all_tags[i]
        previous = all_tags[i - 1] if i > 0 else None
        date = git("log", "-1", "--format=%cs", tag).strip()
        out.append(f"## [{tag[1:]}]({REPO}/releases/tag/{tag}) — {date}\n")
        out.append(release(tag, previous))
    return "\n".join(out)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="fail if CHANGELOG.md is stale")
    ap.add_argument("--release", metavar="TAG", help="print one release's notes")
    args = ap.parse_args()
    if args.release:
        all_tags = tags()
        if args.release not in all_tags:
            print(f"no release tag {args.release}", file=sys.stderr)
            return 1
        i = all_tags.index(args.release)
        sys.stdout.write(release(args.release, all_tags[i - 1] if i > 0 else None))
        return 0
    text = changelog()
    if args.check:
        if not OUT.exists() or OUT.read_text() != text:
            print("CHANGELOG.md is stale: run python3 scripts/changelog.py", file=sys.stderr)
            return 1
        return 0
    OUT.write_text(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
