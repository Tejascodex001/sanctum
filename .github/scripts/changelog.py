#!/usr/bin/env python3
"""Writes the changelog for a release from commit messages.

    changelog.py BUILD OUT_ALL OUT_THIS

OUT_ALL: this build and up to 30 before it, newest first, as
"## Build N" sections (the app shows the ones newer than what's installed).
OUT_THIS: just this build's section (the release notes on GitHub).
A failed build has no tag; its commits roll into the next build.

The notes are for the app's users: commits whose subject starts with
"Internal:", "Docs:" or "CI:" (developer-only changes) are left out.
"""
import re
import subprocess
import sys


def git(*args):
    return subprocess.run(["git", *args], check=True, capture_output=True, text=True).stdout


def commits(rev_range):
    out = git("log", "--reverse", "--format=%x1e%s%x1f%b", rev_range)
    for entry in out.split("\x1e")[1:]:
        subject, _, body = entry.partition("\x1f")
        yield subject.strip(), body


DEVELOPER_ONLY = ("internal:", "docs:", "ci:")


def section(build, rev_range):
    lines = [f"## Build {build}", ""]
    for subject, body in commits(rev_range):
        if not subject or subject.lower().startswith(DEVELOPER_ONLY):
            continue
        lines.append(f"- **{subject}**")
        for raw in body.splitlines():
            if re.match(r"^(Co-Authored-By|Claude-Session|Signed-off-by):", raw, re.I):
                continue
            text = raw.rstrip()
            if text.startswith("- "):
                lines.append(f"  {text}")
            elif text.startswith("  ") and lines[-1].startswith("  - "):
                lines[-1] += " " + text.strip()
    if len(lines) == 2:
        lines.append("- Improvements and fixes")
    return "\n".join(lines) + "\n"


def main():
    build, out_all, out_this = int(sys.argv[1]), sys.argv[2], sys.argv[3]
    tags = sorted(
        (int(t[len("build-"):]), t)
        for t in git("tag", "--list", "build-*").split()
        if t[len("build-"):].isdigit() and int(t[len("build-"):]) < build
    )
    # This build: since the newest earlier release.
    this = section(build, f"{tags[-1][1]}..HEAD" if tags else "HEAD")
    sections = [this]
    for i in range(len(tags) - 1, max(-1, len(tags) - 31), -1):
        number, tag = tags[i]
        rev_range = f"{tags[i - 1][1]}..{tag}" if i > 0 else tag
        sections.append(section(number, rev_range))
    with open(out_this, "w") as f:
        f.write(this)
    with open(out_all, "w") as f:
        f.write("# Sanctum changelog\n\n" + "\n".join(sections))


main()
