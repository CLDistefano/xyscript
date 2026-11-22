#!/usr/bin/env python3
"""
gen-news — scaffold a release-notes section for an upcoming XyScript release.

Run at release prep, BEFORE tagging:

    python packaging/gen-news.py 0.1.17            # since the latest tag
    python packaging/gen-news.py 0.1.17 --since v0.1.15

It gathers the commit subjects since the previous tag (this repo squash-merges each
pull request into one `type: summary (#NN)` commit), keeps the user-facing ones
(`feat:` / `fix:` / `perf:`), and PREPENDS a `## v<version>` section to docs/NEWS.md
pre-filled with one bullet per change. You then TRIM that section to the real
highlights and commit it — the build bakes whatever NEWS.md contains (see
xyscript-app/src/news.rs), so the section you leave is exactly what the "New in
<version>" tab shows after the update.

This is a scaffolding aid, not an automatic changelog: the whole point is that a
human edits the result before it ships. The script refuses to run if a section for
the version already exists, so re-running never duplicates or clobbers.
"""
import io
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, ".."))
NEWS = os.path.join(ROOT, "docs", "NEWS.md")

# Conventional-commit types worth surfacing to end users (chore/refactor/docs/test/ci are dropped).
USER_FACING = ("feat", "fix", "perf")
# `type(scope)!: summary (#NN)` → capture type and summary; tolerate an optional scope and `!`.
COMMIT = re.compile(r"^(?P<type>[a-z]+)(?:\([^)]*\))?!?:\s*(?P<summary>.*?)\s*$")
PR_SUFFIX = re.compile(r"\s*\(#\d+\)\s*$")


def git(*args):
    return subprocess.run(
        ["git", *args], cwd=ROOT, capture_output=True, text=True, check=True
    ).stdout.strip()


def previous_tag(explicit):
    if explicit:
        return explicit
    try:
        return git("describe", "--tags", "--abbrev=0")
    except subprocess.CalledProcessError:
        return None  # no tags yet → whole history


def bullets(since):
    rng = f"{since}..HEAD" if since else "HEAD"
    subjects = git("log", rng, "--no-merges", "--pretty=%s").splitlines()
    out = []
    for s in subjects:
        m = COMMIT.match(s)
        if not m or m.group("type") not in USER_FACING:
            continue
        summary = PR_SUFFIX.sub("", m.group("summary")).strip()
        if summary:
            out.append(summary[0].upper() + summary[1:])
    return out


def main():
    args = [a for a in sys.argv[1:]]
    if not args or args[0] in ("-h", "--help"):
        print(__doc__)
        return 0
    version = "v" + args[0].lstrip("vV")
    since = None
    if "--since" in args:
        since = args[args.index("--since") + 1]
    since = previous_tag(since)

    text = io.open(NEWS, encoding="utf-8").read()
    if re.search(rf"^##\s+v?{re.escape(version.lstrip('v'))}\b", text, re.M | re.I):
        print(f"NEWS.md already has a section for {version} — nothing to do.", file=sys.stderr)
        return 1

    items = bullets(since)
    span = f"since {since}" if since else "(whole history)"
    lines = [f"## {version}", ""]
    lines.append(f"TODO: trim to user-facing highlights before tagging (scaffolded from commits {span}).")
    if items:
        lines += [f"- {b}" for b in items]
    else:
        lines.append("- ")
    section = "\n".join(lines) + "\n\n"

    # Insert before the first existing `## ` section; if there is none, after the file's prose.
    m = re.search(r"^##\s", text, re.M)
    if m:
        new = text[: m.start()] + section + text[m.start():]
    else:
        new = text.rstrip("\n") + "\n\n" + section
    io.open(NEWS, "w", encoding="utf-8", newline="\n").write(new)
    print(f"Prepended a {version} section to docs/NEWS.md ({len(items)} bullet(s), {span}).")
    print("Now edit docs/NEWS.md: trim to highlights and delete the TODO line.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
