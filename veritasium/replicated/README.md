# bevy_replicated

Unopinionated ECS replication substrate for [Bevy].

`bevy_replicated` marks components and singleton resources as replicated,
captures their post-images as ordered, hash-chained transactions, and exposes
transport-neutral packet helpers for multiplayer frameworks to bridge into
their own sessions, channels, and liveness policy.

It is the replication substrate extracted from a peer-to-peer game, designed as
a general-purpose building block for lockstep or rollback-style Bevy projects.
That general-purpose claim is currently demonstrated by a networked Bevy game
and the in-tree grid-painter example; it is **not yet externally
adopted** because no crates.io release exists for outside projects to consume.

> **Status: pre-release, not yet on crates.io.** The crate is feature-complete
> and dogfooded, but it is designed around the veritasium fork's mediated write
> gate (`MediatedAccess` / `MediatedMut`) — a deliberate permanent divergence
> from released Bevy. See
> [Vendor coupling (permanent)](#vendor-coupling-permanent).
> A crates.io publish would require a future released Bevy to independently ship
> an equivalent mediated write gate. The `0.1.x` framing is therefore
> dogfooding-only until that hypothetical convergence occurs and a published
> artifact receives confirmed external adoption. See
> [`docs/dogfooding-release-rationale.md`](docs/dogfooding-release-rationale.md).

[Bevy]: https://bevyengine.org

## What it gives you

- **`#[derive(Replicated)]`** — opt a component into mediated, recorded writes
  (the derive supplies the `Component` impl and an `rkyv` wire codec).
- **`#[derive(ReplicatedResource)]`** — opt a singleton resource into explicit
  replicated insert/modify/remove transactions.
- **`ResourceRegistry::registered()`** — inspect the registered singleton
  resources and their stable `ResourceId` values; duplicate resource ids fail
  at registration time.
- **`MediatedMut<T>` capture** — replicated in-place mutations go through Bevy's
  closure-only `MediatedMut<T>`; the engine captures the post-image and records
  it into the current frame transaction automatically.
- **`ReplicationChain`** — a rolling blake3/sha256 hash over the canonical
  transaction stream, for divergence detection between peers.
- **Canonical ordering** — a deterministic total order over concurrently
  received transactions, so all peers apply them identically.
- **Wire encode/decode** — compact `rkyv`-backed transaction and chain-hash
  envelopes.
- **Generic identity** — the crate is generic over a consumer-supplied
  `NetworkId`; it carries no game-specific identity types.
- **Two shipped passive models** — `CanonicalEventual` for equal-peer
  convergence and `AuthorityOrdered` for single-writer total ordering.
- **Foreign-transport convention** — `interop` drains `GossipOutbox` /
  `ChainHashOutbox` and feeds inbound wire bytes without requiring downstream
  transports to implement a sealed trait. Transport adapters own their session
  queues and depend only on these byte-level helpers.

## Framework-author transport boundary

`bevy_replicated` does not own discovery, sockets, sessions, reconnect
handshakes, matchmaking, or lobby authority. A multiplayer framework integrates
by treating the crate as an ECS transaction engine:

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

Received transactions are buffered, canonically ordered, gap-checked, and
applied; the chain hash can be exchanged between peers to detect divergence.

The current pre-release wire/domain version is v3. This was a coordinated
resource-target cutover: old v1/v2 transaction bytes are rejected loudly, and
the crate keeps representative transaction golden coverage.

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

Fatal apply or flush failures do not panic silently. `ReplicatedPlugin`
initializes a `ReplicationFatalQueue`, logs fatal replication failures, and
queues the typed `ReplicationError` for the embedding game to inspect, drain,
or bridge into its own shutdown policy.

## Architecture

### Inline source-of-truth policy

Two small primitives — a rolling **mutation log** (`src/mutation_log.rs`) and a
schedule **sync barrier** (`src/sync_barrier.rs`) — are **inlined** into this
crate rather than imported from an engine fork. They were prototyped as
`bevy_ecs` modules, but the narrowed deterministic-mutation RFC does **not**
propose them for upstream Bevy, so the inlined copies are their permanent home
here. Both are self-contained against stock Bevy (`mutation_log` uses only
`std` + `blake3`; `sync_barrier` uses only the public `World::flush`).

The copies are kept faithful to their prototype API — including surface this
crate does not currently exercise — so that, if an upstream equivalent ever
lands, they can be swapped out cleanly. This is why `mutation_log` carries a
module-level `#![allow(dead_code)]` (its full primitive surface is retained but
not all of it is used yet); `sync_barrier`'s surface is fully exercised and
needs no such allow.

### Vendor coupling (permanent)

The **one** remaining coupling to a Bevy fork is the *mediated component write
gate*: `MediatedAccess`, `MediatedMutable`, and `MediatedMut`. These give
replicated components a compile-time guarantee that they can only be written
through the mediated `MediatedMut` path (a plain `Query<&mut T>` is rejected at
compile time), which is what makes the recorded transaction stream a complete
audit of every replicated mutation.

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
