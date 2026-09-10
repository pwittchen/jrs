#!/usr/bin/env python3
"""Check relative links and #anchors in the repository's tracked Markdown files.

Usage: check_links.py [FILE.md ...]   (defaults to every tracked *.md)

Prints one line per broken link, `file:line: target — reason`, and exits 1 if
any were found. External (http, https, mailto) links are not checked.
"""

import os
import re
import subprocess
import sys
import unicodedata

LINK = re.compile(r"!?\[(?:[^\[\]]|\[[^\]]*\])*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
FENCE = re.compile(r"^\s*(```|~~~)")
HEADING = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")
HTML_ANCHOR = re.compile(r"<a\s+(?:name|id)=\"([^\"]+)\"")


def slug(text):
    """GitHub's heading anchor: lowercase, drop punctuation, spaces to hyphens."""
    text = re.sub(r"`([^`]*)`", r"\1", text)
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"<[^>]+>", "", text)
    out = []
    for ch in text.lower():
        if ch in " -_" or ch.isalnum() or unicodedata.category(ch).startswith("M"):
            out.append("-" if ch == " " else ch)
    return "".join(out)


def lines_outside_fences(path):
    in_fence = False
    with open(path, encoding="utf-8") as f:
        for number, line in enumerate(f, 1):
            if FENCE.match(line):
                in_fence = not in_fence
                continue
            if not in_fence:
                yield number, line


def anchors(path, cache={}):
    if path not in cache:
        seen, found = {}, set()
        for _, line in lines_outside_fences(path):
            for name in HTML_ANCHOR.findall(line):
                found.add(name)
            m = HEADING.match(line)
            if not m:
                continue
            base = slug(m.group(2))
            count = seen.get(base, 0)
            seen[base] = count + 1
            found.add(base if count == 0 else f"{base}-{count}")
        cache[path] = found
    return cache[path]


def tracked_markdown():
    out = subprocess.run(
        ["git", "ls-files", "*.md"], capture_output=True, text=True, check=True
    )
    return [p for p in out.stdout.splitlines() if p]


def main(files):
    broken = 0
    for path in files:
        for number, line in lines_outside_fences(path):
            # Inline code can hold link-shaped text that is not a link.
            line = re.sub(r"`[^`]*`", "", line)
            for target in LINK.findall(line):
                if re.match(r"^(https?|mailto):", target):
                    continue
                file_part, _, anchor = target.partition("#")
                dest = (
                    os.path.normpath(os.path.join(os.path.dirname(path), file_part))
                    if file_part
                    else path
                )
                reason = None
                if not os.path.exists(dest):
                    reason = "no such file"
                elif anchor and dest.endswith(".md") and anchor not in anchors(dest):
                    reason = f"no heading #{anchor} in {dest}"
                if reason:
                    broken += 1
                    print(f"{path}:{number}: {target} — {reason}")
    return 1 if broken else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:] or tracked_markdown()))
