# Changelog

All notable changes to `bevy_replicated` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Permanent-fork decision (2026-06-10): the two veritasium fork patches
  (observer ordering rewrite + mediated access v2 kernel) are deliberate
  permanent divergences from Bevy, never to be upstreamed. The `stock_bevy`
  feature reservation and its build.rs warning are removed; bevyengine/bevy#24370
  is closed and no longer tracked as a blocker. See
  `docs/src/architecture/bevy-replicated-vendor-coupling.md`.

### Added

- Release-preparation pass ahead of the standalone crates.io extraction:
  crate metadata (description, license, keywords, categories, MSRV), dual
  `MIT OR Apache-2.0` license files, this changelog, and a crate `README`.
- Extracted replication into a workspace crate for use by downstream games.
- `AuthorityOrdered` as a second passive `ConsistencyModel`, with a distinct
  `AuthorityOrderedEngine` resource alias and model-local authority resources.
- `interop` convention surface for foreign transport adapters, plus an
  optional `aeronet-bridge` proof over `aeronet_io` byte sessions.
- Structural capture for registered replicated components: direct safe
  `insert`/overwrite and `remove` on bound entities now record transaction
  mutations through echo-guarded lifecycle hooks.
- A startup completeness assertion for registered replicated components, failing
  loudly if apply wiring, engine-to-wire mapping, or mediated encoder registration
  is incomplete.
- First-class replicated singleton resources: `ResourceId`,
  `ReplicatedResource`, `#[derive(ReplicatedResource)]`,
  `register_replicated_resource`, `ReplicatedResMut`, and explicit replicated
  resource insert/remove helpers.
- Replicated resource ergonomics: `ResourceRegistry::registered()` exposes an
  auditable resource catalog, duplicate `ResourceId` registrations fail loudly,
  and `ReplicatedResMut` now supports `replace` and `record_current`.

### Changed

- The earlier Wave 3 identity cutover was a hard pre-release wire/domain break
  from v1 to v2, so v1 transaction bytes are rejected loudly instead of being
  reinterpreted.
- The resource-target cutover is a second hard pre-release wire/domain break:
  `WIRE_VERSION` and `CHAIN_DOMAIN_VERSION` are both `3`, the chain domain tag
  is `bevy_replicated.transaction_chain.v3`, and v1/v2 transaction bytes are
  rejected loudly instead of being reinterpreted.
- The `mutation_log` and `sync_barrier` primitives are now consumed from
  crate-local inlined modules instead of the vendored Bevy fork
  (`bevy::ecs::mutation_log` / `bevy::ecs::sync_barrier`). The crate now has a
  single remaining fork coupling — the `MediatedAccess` write gate. Chain
  hashes are unaffected (the chain supplies its own hashes via
  `with_genesis_hash` / `append_with_hash`), so no `CHAIN_DOMAIN_VERSION` bump
  is required.
- The FrameRollback acid test recorded a Boundary verdict, so rollback is
  documented as a sibling adapter rather than a shipped layer-3 strategy.

### Notes

- **Not yet published.** Publication to crates.io is deferred by design: the
  crate depends on the veritasium Bevy fork for the mediated component write
  gate (`MediatedAccess` / `MediatedMutable` / `MediatedMut`), which is not
  inlinable and has no released-Bevy equivalent. This gate is a permanent
  fork divergence — it will never be upstreamed (see the 2026-06-10 decision
  under [Unreleased]) — so a crates.io publish would become possible only if
  a future released Bevy independently shipped an equivalent gate, which is
  not a tracked event. See
  `docs/src/architecture/bevy-replicated-vendor-coupling.md`.

## [0.1.0] — unreleased

Initial extraction target. The consumer-facing API (`Replicated`,
`ReplicatedComponent`, `ReplicatedPlugin`, `ReplicatedAppExt`, `EntityReplicas`,
`receive_transaction`, `commit_now`) is
considered stable across the extraction; the crate is generic over a
consumer-supplied `NetworkId` and carries no game-specific identity types.
