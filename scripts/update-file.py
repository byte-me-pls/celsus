#!/usr/bin/env python3
"""Writes the file an installed Celsus reads to update itself.

    python3 scripts/update-file.py <tag> <folder with the release's .sig files> > latest.json

Each installer the app can replace itself with has a signature beside it (`<file>.sig`). This puts the signatures
and the addresses of the files together, one entry per system. The repository comes from GITHUB_REPOSITORY.
"""
import datetime
import json
import os
import pathlib
import sys

# Which systems an installer is for, by how its name ends.
SYSTEMS = {
    "-setup.exe": ["windows-x86_64"],
    ".app.tar.gz": ["darwin-aarch64", "darwin-x86_64"],
    ".AppImage": ["linux-x86_64"],
}


def main():
    tag, folder = sys.argv[1], pathlib.Path(sys.argv[2])
    repo = os.environ.get("GITHUB_REPOSITORY", "byte-me-pls/celsus")
    platforms = {}
    for sig in sorted(folder.glob("*.sig")):
        name = sig.name[: -len(".sig")]
        systems = next((s for end, s in SYSTEMS.items() if name.endswith(end)), None)
        if systems is None:
            continue
        entry = {
            "signature": sig.read_text(encoding="utf-8").strip(),
            "url": f"https://github.com/{repo}/releases/download/{tag}/{name}",
        }
        for system in systems:
            platforms[system] = entry
    if not platforms:
        sys.exit("no signed installers: nothing to write")
    json.dump(
        {
            "version": tag.lstrip("v"),
            "notes": f"https://github.com/{repo}/blob/{tag}/CHANGELOG.md",
            "pub_date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "platforms": platforms,
        },
        sys.stdout,
        indent=2,
    )
    print()


main()
