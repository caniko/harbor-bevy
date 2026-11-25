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
STEAM_BEVY_ADAPTER = "bevy-steamworks"
STEAM_GIT_PINS = {
    ("steam-pressure",
     "git+https://github.com/caniko/rs-steam-pressure.git"
     "?rev=bd0ab9786c1bf694f14254034a8ecffda13c1d27#bd0ab9786c1bf694f14254034a8ecffda13c1d27"),
    (STEAM_BEVY_ADAPTER,
     "git+https://github.com/caniko/bevy-steamworks.git"
     "?rev=493cce8779e742bd56e6359a94f10a6196a85dda#493cce8779e742bd56e6359a94f10a6196a85dda"),
}


def check_git_pins(workspace, location, pkgs, expected):
    actual = {(p["name"], p["source"]) for p in pkgs if (p.get("source") or "").startswith("git+")}
    require(actual == expected,
            f"{workspace}: {location} Git pins: missing {sorted(expected - actual)}; "
            f"unexpected {sorted(actual - expected)}")


def check_stock_like(workspace, expected_locals, allowed_local_patches, git_pins=frozenset()):
    manifest = read_toml(workspace / "Cargo.toml")
    lock = read_toml(workspace / "Cargo.lock")
    expected_patches = {"crates-io": allowed_local_patches} if allowed_local_patches else {}
    require(manifest.get("patch", {}) == expected_patches and not lock.get("patch"),
            f"{workspace}: unexpected patches in manifest or lockfile")
    check_git_pins(workspace, "lockfile", lock["package"], git_pins)
    graph = packages(workspace)
    check_git_pins(workspace, "resolved graph",
                   (p for ids in graph.values() for p in ids.values()), git_pins)
    require("bevy" in graph, f"{workspace}: Bevy not resolved")
    bevy_count = 0
    for name, ids in graph.items():
        if name in ENGINE or name.startswith("bevy"):
            require(len(ids) == 1, f"{workspace}: duplicate {name}: {list(ids)}")
            pkg = next(iter(ids.values()))
            if name == STEAM_BEVY_ADAPTER and git_pins:
                require(pkg["version"] == "0.18.0", f"{workspace}: expected {name} 0.18.0: {pkg['id']}")
            else:
                require((pkg["version"], pkg["source"]) == ("0.20.1", CRATES_IO),
                        f"{workspace}: expected crates.io Bevy 0.20.1 for {name}: {pkg['id']}")
                bevy_count += 1
    for expected in expected_locals:
        ids = graph[expected]
        require(len(ids) == 1 and next(iter(ids.values()))["source"] is None,
                f"{workspace}: expected one local {expected}: {list(ids)}")
    return bevy_count


stock_locals = (
    "veritasium_wire", "thespan", "thespan_bevy", "harbor-bevy-core", "harbor-bevy-ui",
    "harbor-bevy-diagnostics", "harbor-bevy-telemetry", "harbor-bevy",
)
stock_bevy = check_stock_like(
    Path("."),
    stock_locals,
    STOCK_ALLOWED_LOCAL_PATCHES,
)
print(
    f"Stock: no Git dependencies or Veritasium patches; only local libp2p path patches; "
    f"{stock_bevy} crates.io Bevy 0.20.1 identities; one local ID each for {', '.join(stock_locals)}",
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

steam_locals = ("harbor-bevy-steam", "harbor-bevy-core", "platform-attestation")
steam_bevy = check_stock_like(Path("steam"), steam_locals, {}, STEAM_GIT_PINS)
print(
    f"Steam: no patches; exactly two pinned Git dependencies (steam-pressure and bevy-steamworks 0.18.0); "
    f"{steam_bevy} crates.io Bevy 0.20.1 identities; one local ID each for {', '.join(steam_locals)}",
    flush=True,
)

tools = Path("crates/tools")
tools_manifest = read_toml(tools / "Cargo.toml")
tools_lock = read_toml(tools / "Cargo.lock")
require(not tools_manifest.get("patch") and not tools_lock.get("patch"),
        "crates/tools: unexpected patches")
tools_graph = packages(tools)
for name, ids in tools_graph.items():
    if name in ENGINE or name.startswith("bevy"):
        require(len(ids) == 1, f"crates/tools: duplicate {name}: {list(ids)}")
for name in ("harbor-bevy-xtask", "harbor-bevy-mcp"):
    ids = tools_graph[name]
    require(len(ids) == 1 and next(iter(ids.values()))["source"] is None,
            f"crates/tools: expected one local {name}: {list(ids)}")
default_tools = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--locked", "--format-version", "1"],
    cwd=tools,
))
default_nodes = {n["id"] for n in default_tools["resolve"]["nodes"]}
default_pkgs = {
    p["name"]: p.get("license") or ""
    for p in default_tools["packages"]
    if p["id"] in default_nodes
}
require("steampipe" not in default_pkgs,
        "crates/tools: steampipe activated under default features")
eupl_pkgs = [name for name, lic in default_pkgs.items() if "EUPL" in lic.upper()]
require(not eupl_pkgs,
        f"crates/tools: EUPL packages activated under default features: {eupl_pkgs}")
print("Tools: isolated workspace; no patches; default features exclude steampipe/EUPL; one local xtask/mcp")
PY
