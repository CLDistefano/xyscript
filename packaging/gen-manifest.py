#!/usr/bin/env python3
"""Generate `manifest.json` for a staged XyScript release bundle.

Walks the bundle directory and records every file's path (relative to the bundle root, forward
slashes) and its SHA-256. The auto-updater compares this against the hashes of the installed files to
tell a pristine shipped file from one the user edited, so it never clobbers edits — and it verifies
downloaded assets against these hashes before applying them.

`manifest.json` lands at the bundle root and is shipped INSIDE the bundle, so after install it is the
baseline the *next* update compares against. It never lists itself.

Usage: gen-manifest.py <bundle-dir> [<release-tag>]
"""
import hashlib
import json
import os
import sys


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: gen-manifest.py <bundle-dir> [<release-tag>]", file=sys.stderr)
        return 2
    bundle = sys.argv[1]
    tag = sys.argv[2] if len(sys.argv) > 2 else ""
    if not os.path.isdir(bundle):
        print(f"not a directory: {bundle}", file=sys.stderr)
        return 2

    files = {}
    for root, _dirs, names in os.walk(bundle):
        for name in names:
            full = os.path.join(root, name)
            rel = os.path.relpath(full, bundle).replace(os.sep, "/")
            if rel == "manifest.json":
                continue  # a manifest cannot hash itself
            h = hashlib.sha256()
            with open(full, "rb") as fh:
                for chunk in iter(lambda: fh.read(1 << 16), b""):
                    h.update(chunk)
            files[rel] = h.hexdigest()

    manifest = {
        "manifest_version": 1,
        "release": tag,
        "files": dict(sorted(files.items())),
    }
    out = os.path.join(bundle, "manifest.json")
    with open(out, "w", encoding="utf-8", newline="\n") as fh:
        json.dump(manifest, fh, indent=2, ensure_ascii=False)
        fh.write("\n")
    print(f"wrote {out}: {len(files)} files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
