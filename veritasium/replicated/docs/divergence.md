# Replication Divergence Contract

`bevy_replicated` keeps a canonical transaction hash chain per replicated
session. After accepting and applying a transaction in canonical order, it
hashes the canonical rkyv bytes plus the transaction's `peer_id` and `sequence`.

Peers that apply the same transaction prefix should have the same
`(applied_count, hash)`. A mismatch means they disagree on the replicated
history. The library emits a `DivergenceEvent` but doesn't recover automatically.

## Checkpoints

Peers exchange `ChainHashReport` messages:

- `peer_id`: the peer that produced the report
- `applied_count`: the transaction count at the checkpoint
- `hash`: the cumulative hash at that count

By default, peers report every 64 applied transactions. Change the cadence
with `ChainHashConfig::transaction_cadence`; `0` disables count-based reports.

The library doesn't know the application's state graph, so opt in to
phase-boundary reports per Bevy state type:

```rust
app.exchange_chain_hash_on_state_transition::<GameState>();
```

Callers can also force a checkpoint with `request_chain_hash_report(world)` or
by writing a `ChainHashCheckpoint` message.

## Divergence

`receive_chain_hash_report(world, report)` compares the remote report to the
local chain. If the local peer hasn't reached `report.applied_count`, it caches
the report and checks it after later transactions apply. If the hashes differ,
the library triggers and writes `DivergenceEvent` with:

- `local_hash`
- `remote_hash`
- `remote_peer`
- `applied_count_at_check`
- `last_common_count`, when an earlier matching checkpoint is known

The first mismatch for a remote peer and `applied_count` sets the status to
`Mismatched`. For that peer and count, the status remains `Mismatched`; no
further `DivergenceEvent` is triggered or written.

Expected divergence:

- Peers are running incompatible application protocol versions.
- A caller intentionally allows one peer to continue after a known mismatch.
- A report for a future count is pending catch-up, not divergence before the
  local chain reaches that count.

Unexpected divergence:

- Normal equal-peer gameplay after Phase 06 ordering has converged.
- Serialization drift between platforms.
- Mutating replicated state outside the transaction apply path.
- Applying the same transaction keys with different mutation payloads.

## Caller Policy

On `DivergenceEvent`, the application should choose a recovery policy;
`bevy_replicated` does not choose for it:

- Drop the session.
- Request a full state resync.
- Surface an error to the user.
- Halt immediately in debug/test builds.
- Log and continue only for audit or diagnostic applications that explicitly
  tolerate divergent replicas.

## Determinism

The chain hashes transaction data, not ECS state, avoiding differences in
reflection ordering, archetype ordering, and scheduler timing.

Local tests verify deterministic rkyv round-trip bytes for `ChainHashReport`
and identical hashes when peers apply the same transaction sequence. This
phase was validated on the current development host; cross-architecture
validation still needs to run on every supported target. ARM validation
remains a downstream CI/device responsibility.
