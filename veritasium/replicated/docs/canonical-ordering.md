# Canonical Transaction Ordering

`bevy_replicated` orders transactions by:

1. `Transaction::sequence`
2. the byte value of `ReplicationPeerId`

This is the lexicographic order of `(sequence, peer_id)`. The rule is pure:
it does not inspect receive time, `HashMap` iteration order, local frame number,
wall-clock time, transport metadata, or ECS state.

Every peer maintains a monotonic local sequence counter for the peer id assigned
to the current replication session. A `(peer_id, sequence)` pair identifies one
transaction. Reusing the same pair for a different transaction is a protocol
error. Receiving the same transaction twice is treated as a duplicate delivery
and ignored.

Incoming transactions are buffered per peer by sequence. A transaction is
eligible to apply only when it is the next contiguous sequence for its peer and
all active peers have supplied every lower canonical key. At each apply step,
the buffer chooses the lowest unblocked `(sequence, peer_id)` pair by the
canonical ordering rule and applies that transaction. This repeats until no
eligible transaction remains.

For known equal-peer gameplay, the application should configure the fixed
participant set before replicated gameplay starts. For a two-player game, use
the identities assigned by peer negotiation, not the lobby host. If no
fixed participant set is configured, the library uses dynamic discovery for
single-process tests and local tools: peers become active when a transaction
from that peer is observed.

Out-of-order receipt is therefore harmless when the missing lower sequence later
arrives: the higher sequence stays buffered. Lost transactions are detected but
not recovered in Phase 06. If a buffer observes sequence `N` while still waiting
for a lower sequence for the same peer, the gap timer starts. After the default
200 ms timeout, the crate records a `TransactionGapAlert`. If a transaction is
blocked because another active peer has not supplied a lower canonical key, the
alert includes `blocked_by` with the transaction that exposed the missing key.
Recovery with nack, no-op watermarks, or gossip redundancy is intentionally
deferred to a later phase.
