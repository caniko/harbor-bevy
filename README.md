# harbor-bevy

Bevy 0.20.1 tooling and replication crates. The minimum supported Rust
version is 1.97.1. Licensed under MIT OR Apache-2.0.

The three Cargo workspaces have separate manifests and lockfiles:

| Workspace | Location | Purpose |
| --- | --- | --- |
| Stock | `/` | Bevy-free `veritasium_wire`, with no Git dependencies or Bevy patches |
| Fork | `/veritasium` | Replication, derives, and the grid-painter example on pinned Veritasium Bevy 0.20.1 |
| Interim Steam | `/steam` | Unpublished isolation stub until the Steam dependencies are released |

Run `cargo check --workspace --tests` and `cargo test --workspace` from each
workspace. From the repository root, run `scripts/check-denylist.sh`,
`scripts/check-single-identity.sh`, and `nix flake check`.

The Nix flake exports helpers for native Bevy dependencies, a pinned Linux
visual-test environment, and development shells. Deployment and Steam
tooling are optional packages supplied by the caller, not flake inputs.
