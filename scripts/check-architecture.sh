#!/usr/bin/env bash
# Verify that the public architecture inventory exactly matches the Rust and Bun
# workspace packages. This catches both missing members and stale table entries.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

python3 - "$ROOT" <<'PY'
import glob
import json
import re
import subprocess
import sys
from pathlib import Path

root = Path(sys.argv[1])
metadata = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--no-deps", "--format-version", "1"],
    cwd=root,
    text=True,
))
workspace_ids = set(metadata["workspace_members"])
expected: set[str] = {
    package["name"] for package in metadata["packages"] if package["id"] in workspace_ids
}

package_json = json.loads((root / "package.json").read_text())
for pattern in package_json["workspaces"]:
    for member in glob.glob(str(root / pattern)):
        manifest_path = Path(member) / "package.json"
        if manifest_path.is_file():
            expected.add(json.loads(manifest_path.read_text())["name"])

architecture = (root / "docs" / "architecture.md").read_text()
section = architecture.split("## 1. Service inventory", 1)[1].split("## 2.", 1)[0]
listed = set(re.findall(r"^\| `([^`]+)`", section, re.MULTILINE))
project_listed = {name for name in listed if name.startswith("nisaba-") or name.startswith("@nisaba/")}

missing = sorted(expected - project_listed)
stale = sorted(project_listed - expected)
if missing or stale:
    for name in missing:
        print(f"ERROR: workspace package missing from architecture inventory: {name}")
    for name in stale:
        print(f"ERROR: stale package in architecture inventory: {name}")
    raise SystemExit(1)

print(f"docs/architecture.md inventory matches {len(expected)} workspace packages.")
PY
