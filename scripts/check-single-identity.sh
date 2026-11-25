#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
exec python3 - <<'PY'
import json
import subprocess
import tomllib
from collections import defaultdict
from pathlib import Path

URL = "https://codeberg.org/caniko/rs-veritasium.git"
REV = "23181dcb456a5f175f6fabcdebbf4a5f5d12f4cf"
SOURCE = f"git+{URL}?rev={REV}#{REV}"
ENGINE = set(Path("scripts/bevy-0.20.1-packages.txt").read_text().splitlines())


def require(condition, message):
    if not condition:
        raise SystemExit(message)


def read_toml(path):
    return tomllib.loads(path.read_text())


def packages(workspace):
    data = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--all-features", "--format-version", "1"],
        cwd=workspace,
    ))
    by_name = defaultdict(dict)
    for pkg in data["packages"]:
        by_name[pkg["name"]][pkg["id"]] = pkg
    return by_name


def unpatched(workspace, expected):
    manifest = read_toml(workspace / "Cargo.toml")
    lock = read_toml(workspace / "Cargo.lock")
    require(not manifest.get("patch") and not lock.get("patch"),
            f"{workspace}: unexpected patches")
    require(all(not p.get("source", "").startswith("git+") for p in lock["package"]),
            f"{workspace}: Git dependency in lockfile")
    graph = packages(workspace)
    for name, ids in graph.items():
        require(not any((p["source"] or "").startswith("git+") for p in ids.values()),
                f"{workspace}: Git dependency {name}")
        if name.startswith("bevy"):
            require(len(ids) == 1, f"{workspace}: duplicate {name}: {list(ids)}")
    ids = graph[expected]
    require(len(ids) == 1 and next(iter(ids.values()))["source"] is None,
            f"{workspace}: expected one local {expected}: {list(ids)}")


unpatched(Path("."), "veritasium_wire")
print("Stock: no Git dependencies or patches; one local veritasium_wire", flush=True)

fork = Path("veritasium")
patches = read_toml(fork / "Cargo.toml")["patch"]["crates-io"]
require(len(ENGINE) == 74 and "bevy-settings" in ENGINE, "Invalid engine package inventory")
require(patches.keys() == ENGINE,
        f"Fork patch coverage: missing {sorted(ENGINE - patches.keys())}; "
        f"unexpected {sorted(patches.keys() - ENGINE)}")
for name, pin in patches.items():
    require(pin == {"git": URL, "rev": REV}, f"Wrong fork pin for {name}: {pin}")

graph = packages(fork)
resolved_engine = ENGINE & graph.keys()
require("bevy" in resolved_engine, "Fork: Bevy not resolved")
for name in sorted(resolved_engine):
    ids = graph[name]
    require(len(ids) == 1, f"Fork: duplicate {name}: {list(ids)}")
    pkg = next(iter(ids.values()))
    require((pkg["version"], pkg["source"]) == ("0.20.1", SOURCE),
            f"Fork: wrong version or source for {name}: {pkg['id']}")
for name in ("veritasium_wire", "veritasium_replicated", "veritasium_replicated_macros"):
    ids = graph[name]
    require(len(ids) == 1 and next(iter(ids.values()))["source"] is None,
            f"Fork: expected one local {name}: {list(ids)}")
print(f"Fork: 74 patches; {len(resolved_engine)} resolved Bevy identities; one of each local crate",
      flush=True)

unpatched(Path("steam"), "harbor-bevy-steam")
print("Steam: no Git dependencies or patches; one local harbor-bevy-steam")
PY
