#!/usr/bin/env python3
"""Writes the table of Rust crates at the end of THIRD-PARTY-NOTICES.md from Cargo.lock,
reading each crate's license and repository from its manifest in the local Cargo registry.
Run it after a dependency changes: `python3 scripts/third-party-crates.py`."""

import glob
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MARK = "<!-- crates -->"

lock = open(os.path.join(ROOT, "Cargo.lock"), encoding="utf-8").read()
packages = re.findall(r'\[\[package\]\]\nname = "([^"]+)"\nversion = "([^"]+)"\nsource = "registry[^"]*"', lock)
registries = glob.glob(os.path.expanduser("~/.cargo/registry/src/*/"))
rows, missing = [], []
for name, version in sorted(packages):
    manifest = next((p for r in registries if os.path.exists(p := f"{r}{name}-{version}/Cargo.toml")), None)
    if not manifest:
        missing.append(f"{name} {version}")
        continue
    text = open(manifest, encoding="utf-8", errors="replace").read()
    license = re.search(r'^license\s*=\s*"([^"]+)"', text, re.M)
    repository = re.search(r'^repository\s*=\s*"([^"]+)"', text, re.M)
    rows.append(f"| {name} | {version} | {license.group(1) if license else '(see its LICENSE file)'} | {repository.group(1) if repository else ''} |")
if missing:
    sys.exit("not in the local registry (run `cargo fetch` first): " + ", ".join(missing))

path = os.path.join(ROOT, "THIRD-PARTY-NOTICES.md")
notices = open(path, encoding="utf-8").read()
head, _, _ = notices.partition(MARK)
table = "\n".join(["| Crate | Version | License | Source |", "|---|---|---|---|", *rows])
open(path, "w", encoding="utf-8").write(f"{head}{MARK}\n{len(rows)} crates, as pinned in `Cargo.lock`.\n\n{table}\n")
print(f"{len(rows)} crates written to THIRD-PARTY-NOTICES.md")
