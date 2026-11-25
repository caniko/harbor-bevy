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


CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
STOCK_ALLOWED_LOCAL_PATCHES = {
    "libp2p-swarm": {"path": "vendor/libp2p-swarm-0.47.1"},
    "libp2p-relay": {"path": "vendor/libp2p-relay-0.21.1"},
    "libp2p-dns": {"path": "crates/net/libp2p-dns-disabled", "version": "0.44.0"},
    "libp2p-mdns": {"path": "crates/net/libp2p-mdns-disabled", "version": "0.48.0"},
}


def check_stock_like(workspace, expected_locals, allowed_local_patches):
    manifest = read_toml(workspace / "Cargo.toml")
    lock = read_toml(workspace / "Cargo.lock")
    patch_tables = manifest.get("patch", {})
    if allowed_local_patches:
        require(set(patch_tables.keys()) <= {"crates-io"},
                f"{workspace}: unexpected patch sources: {sorted(patch_tables)}")
        crates_io_patches = patch_tables.get("crates-io", {})
        require(crates_io_patches == allowed_local_patches,
                f"{workspace}: unexpected [patch.crates-io]: {crates_io_patches}")
        require(not (set(crates_io_patches) & ENGINE),
                f"{workspace}: Bevy engine crate patched in stock workspace")
    else:
        require(not patch_tables and not lock.get("patch"),
                f"{workspace}: unexpected patches")
    require(all(not p.get("source", "").startswith("git+") for p in lock["package"]),
            f"{workspace}: Git dependency in lockfile")
    graph = packages(workspace)
    for name, ids in graph.items():
        require(not any((p["source"] or "").startswith("git+") for p in ids.values()),
                f"{workspace}: Git dependency {name}")
        if name in ENGINE or name.startswith("bevy"):
            require(len(ids) == 1, f"{workspace}: duplicate {name}: {list(ids)}")
            pkg = next(iter(ids.values()))
            if name in ENGINE:
                require((pkg["version"], pkg["source"]) == ("0.20.1", CRATES_IO),
                        f"{workspace}: expected crates.io Bevy 0.20.1 for {name}: {pkg['id']}")
    for expected in expected_locals:
        ids = graph[expected]
        require(len(ids) == 1 and next(iter(ids.values()))["source"] is None,
                f"{workspace}: expected one local {expected}: {list(ids)}")
    return len(ENGINE & graph.keys())


stock_bevy = check_stock_like(
    Path("."),
    ("veritasium_wire", "thespan", "thespan_bevy"),
    STOCK_ALLOWED_LOCAL_PATCHES,
)
print(
    f"Stock: no Git dependencies; only local libp2p path patches; "
    f"{stock_bevy} crates.io Bevy 0.20.1 identities; one local veritasium_wire/thespan/thespan_bevy",
    flush=True,
)

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

check_stock_like(Path("steam"), ("harbor-bevy-steam",), {})
print("Steam: no Git dependencies or patches; one local harbor-bevy-steam")
PY
