# bevy_replicated

Unopinionated ECS replication substrate for [Bevy].

`bevy_replicated` marks components and singleton resources as replicated,
captures their post-images as ordered, hash-chained transactions, and exposes
transport-neutral packet helpers for multiplayer frameworks to bridge into
their own sessions, channels, and liveness policy.

Extracted from a peer-to-peer game, the crate is a general-purpose building
block for lockstep or rollback-style Bevy projects. A networked Bevy game and
the in-tree grid-painter example demonstrate that use. There is no crates.io
release for outside projects to consume, so there is **no external adoption yet**.

> **Status: pre-release, not yet on crates.io.** The crate is feature-complete
> and dogfooded, but it is designed around the veritasium fork's mediated write
> gate (`MediatedAccess` / `MediatedMut`) — a deliberate permanent divergence
> from released Bevy. See
> [Vendor coupling (permanent)](#vendor-coupling-permanent).
> A crates.io release requires Bevy to independently ship an equivalent gate.
> The `0.1.x` framing stays dogfooding-only unless that happens and external
> adoption of a published artifact is confirmed. See
> [`docs/dogfooding-release-rationale.md`](docs/dogfooding-release-rationale.md).

[Bevy]: https://bevyengine.org

## What it gives you

- **`#[derive(Replicated)]`** — opt a component into mediated, recorded writes
  (the derive supplies the `Component` impl and an `rkyv` wire codec).
- **`#[derive(ReplicatedResource)]`** — opt a singleton resource into explicit
  replicated insert/modify/remove transactions.
- **`ResourceRegistry::registered()`** — list registered singleton resources
  and their stable `ResourceId` values. Registration rejects duplicate IDs.
- **`MediatedMut<T>` capture** — mutate in place through Bevy's closure-only
  `MediatedMut<T>`; the engine records the post-image in the current frame
  transaction automatically.
- **`ReplicationChain`** — a rolling blake3/sha256 hash over the canonical
  transaction stream, for divergence detection between peers.
- **Canonical ordering** — a deterministic total order over concurrently
  received transactions, so all peers apply them identically.
- **Wire encode/decode** — compact `rkyv`-backed transaction and chain-hash
  envelopes.
- **Generic identity** — you can supply your own `NetworkId`; the crate has no
  game-specific identity types.
- **Two shipped passive models** — `CanonicalEventual` for equal-peer
  convergence and `AuthorityOrdered` for single-writer total ordering.
- **Foreign-transport convention** — `interop` drains `GossipOutbox` /
  `ChainHashOutbox` and feeds inbound wire bytes without requiring downstream
  transports to implement a sealed trait. Transport adapters own their session
  queues and depend only on these byte-level helpers.

## Framework-author transport boundary

Use `bevy_replicated` as an ECS transaction engine. `bevy_replicated` does not
own discovery, sockets, sessions, reconnect handshakes, matchmaking, or lobby
authority.

1. Configure `ReplicationIdentity` for the local peer and, for fixed equal-peer
   sessions, configure `ReplicationParticipants` before gameplay starts.
2. Register every replicated component/resource with `ReplicatedAppExt`.
3. Bind local Bevy entities to stable `NetworkId` values in `EntityReplicas`.
4. Mutate replicated components through `MediatedMut<T>` and resources through
   `ReplicatedResMut<T>` or the explicit insert/remove helpers. `modify`,
   `replace`, and `record_current` emit `ModifyResource`; insert/remove helpers
   emit `InsertResource` and `RemoveResource` respectively.
5. Drain outbound packets with `bevy_replicated::interop::drain_world_outbound_packets`
   or `drain_outbound_packets`, then send those bytes over the framework's
   reliable ordered gameplay channel.
6. Feed received bytes back through `receive_inbound_packet`.
7. Drain `ReplicationFatalQueue` or observe `DivergenceEvent` and translate
   failures into the framework's own disconnect/shutdown policy.

The stable v1 interop surface is intentionally convention-based:
`ChannelSupport`, `OutboundReplicationPacket`, `InboundReplicationPacket`,
`drain_outbound_packets`, `drain_world_outbound_packets`, and
`receive_inbound_packet`. Unsupported required channel kinds are hard errors;
bridges must never silently downgrade reliable ordered replication traffic.

## Conceptual usage

```rust,ignore
use bevy::prelude::*;
use bevy_replicated::{
    Replicated, ReplicatedAppExt, ReplicatedPlugin, ReplicatedResMut,
    ReplicatedResource,
};

// 1. Mark a component as replicated. The `Replicated` derive supplies the
//    `Component` impl itself (do NOT also derive `Component`) and round-trips
//    the component through rkyv, so the type must be rkyv-serializable.
#[derive(Replicated, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[replicated(component_id = 1)]
struct Health(u32);

#[derive(ReplicatedResource, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[replicated(resource_id = 2)]
struct MatchClock(u32);

// 2. Add the plugin and register the component. The crate is generic over a
//    `NetworkId`; this example uses the built-in `OpaqueId` default.
fn setup(app: &mut App) {
    app.add_plugins(ReplicatedPlugin::default());
    <App as ReplicatedAppExt>::register_replicated_component::<Health>(app);
    <App as ReplicatedAppExt>::register_replicated_resource::<MatchClock>(app);
}

// 3. Mutate through `MediatedMut::modify` — the write is captured into the
//    current frame transaction and folded into the replication chain hash.
#[derive(Resource)]
struct DamageTarget(Entity);

fn damage(
    mut healths: bevy::ecs::system::MediatedMut<Health>,
    target: Res<DamageTarget>,
) {
    healths
        .modify(target.0, |h| h.0 = h.0.saturating_sub(1))
        .expect("target has Health");
}

fn tick_clock(mut clock: ReplicatedResMut<MatchClock>) {
    clock
        .modify(|clock| clock.0 += 1)
        .expect("clock resource exists");
}

fn reset_clock(mut clock: ReplicatedResMut<MatchClock>) {
    let _previous = clock
        .replace(MatchClock(0))
        .expect("clock replacement records");
}
```

The crate buffers received transactions, orders them canonically, checks for
gaps, and applies them. Peers can exchange chain hashes to detect divergence.

The pre-release wire/domain version is v3, a coordinated resource-target
cutover. The crate explicitly rejects v1/v2 transaction bytes and retains
golden tests for representative transactions.

`FrameRollback` is intentionally **not** listed as a shipped layer-3 model. The
in-tree acid test rendered a Boundary verdict: rollback can consume lower-layer
conventions where useful, but its GGRS frame loop and snapshot store sit beside
the forward-only `ReplicationChain`.

## Error architecture

`bevy_replicated` exposes typed, source-preserving errors. The public
`ReplicationError` covers missing entity mappings, unknown components,
conflicting transaction keys, canonical-ordering failures, wire failures, and
vendored Bevy mediated-access failures. The public `WireError` covers `rkyv`
encode/decode failures, incompatible wire versions, and
`WireError::InvariantBroken` for internal replication invariant reports. The
`rkyv`, canonical-ordering, and mediated-access variants preserve their original
`Error::source()` chain.

`ReplicatedPlugin` initializes a `ReplicationFatalQueue`. On fatal apply or
flush failures, it logs the failure rather than panicking silently and queues
the typed `ReplicationError` for the game to inspect, drain, or pass to its
shutdown policy.

## Architecture

### Inline source-of-truth policy

The rolling **mutation log** (`src/mutation_log.rs`) and schedule **sync
barrier** (`src/sync_barrier.rs`) live here permanently, not in an engine
fork. They began as `bevy_ecs` modules, but the narrowed deterministic-mutation
RFC does not propose them for upstream Bevy. Both work with stock Bevy:
`mutation_log` uses only `std` + `blake3`, and `sync_barrier` uses only the public
`World::flush`.

Both retain the full prototype API so an upstream equivalent could replace
them cleanly. `mutation_log` uses a module-level `#![allow(dead_code)]` for the
parts this crate doesn't use yet. The crate uses all of `sync_barrier`, so it
needs no such allowance.

### Vendor coupling (permanent)

Only the *mediated component write gate* requires a Bevy fork: `MediatedAccess`,
`MediatedMutable`, and `MediatedMut`. The compiler requires writes to replicated
components to go through `MediatedMut` and rejects plain `Query<&mut T>`. The
recorded transaction stream thus audits every replicated mutation.

This gate is **not inlinable** into a downstream crate: it adds a variant to the
*sealed* `ComponentMutability` trait, adds an associated const to the
`Component` trait, and implements a `QueryData` over private `bevy_ecs`
internals. Reproducing it requires forking `bevy_ecs` — exactly what
the [Veritasium fork](https://codeberg.org/caniko/rs-veritasium) does with its
observer-ordering and mediated-access patches. The isolated fork workspace pins
the fork revision in its `Cargo.toml`.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
