# Replication Divergence Contract

`bevy_replicated` maintains a canonical transaction hash chain for every
replicated session. The chain hashes the canonical rkyv bytes of each applied
transaction, plus the transaction's `peer_id` and `sequence`, after the
transaction has been accepted and applied in canonical order.

Two peers should have the same `(applied_count, hash)` when they have applied
the same transaction prefix. A mismatch means the peers disagree about the
replicated transaction history. The library reports that condition by emitting a
`DivergenceEvent`; it does not recover automatically.

## Checkpoints

Peers exchange `ChainHashReport` messages:

- `peer_id`: the peer that produced the report
- `applied_count`: the transaction count at the checkpoint
- `hash`: the cumulative hash at that count

The default transaction cadence is every 64 applied transactions. Apps can
change this with `ChainHashConfig::transaction_cadence`; setting it to `0`
disables count-based reports.

Phase-boundary reports are opt-in per Bevy state type because this library does
not know an application's state graph. Register them with:

```rust
app.exchange_chain_hash_on_state_transition::<GameState>();
```

Callers can also force a checkpoint with `request_chain_hash_report(world)` or
by writing a `ChainHashCheckpoint` message.

## Divergence

`receive_chain_hash_report(world, report)` compares a remote report to the
local chain. If the local peer has not reached `report.applied_count` yet, the
report is cached and checked after later transactions apply. If the hashes
differ, the library triggers and writes `DivergenceEvent` with:

- `local_hash`
- `remote_hash`
- `remote_peer`
- `applied_count_at_check`
- `last_common_count`, when an earlier matching checkpoint is known

Repeated mismatching reports from the same remote peer at the same
`applied_count` are suppressed after the first event. The status remains
`Mismatched`, but observers are not spammed with duplicate diagnostics for one
checkpoint.

Expected divergence:

- Peers are running incompatible application protocol versions.
- A caller intentionally allows one peer to continue after a known mismatch.
- A peer receives a report for a future count; this is pending catch-up, not a
  divergence until the local chain reaches that count.

Unexpected divergence:

- Normal equal-peer gameplay after Phase 06 ordering has converged.
- Serialization drift between platforms.
- Mutating replicated state outside the transaction apply path.
- Applying the same transaction keys with different mutation payloads.

## Caller Policy

On `DivergenceEvent`, application code should choose a recovery policy:

- Drop the session.
- Request a full state resync.
- Surface an error to the user.
- Halt immediately in debug/test builds.
- Log and continue only for audit or diagnostic applications that explicitly
  tolerate divergent replicas.

`bevy_replicated` deliberately does not choose among these policies.

## Determinism

The chain input is transaction-side data, not ECS state. This avoids reflection
ordering, archetype ordering, and scheduler timing differences.

The local test suite verifies deterministic rkyv round-trip bytes for
`ChainHashReport` and identical hashes for peers applying the same transaction
sequence. Cross-architecture validation must still be run on each supported
target; this phase was validated locally on the current development host, and
ARM validation remains a downstream CI/device responsibility.
