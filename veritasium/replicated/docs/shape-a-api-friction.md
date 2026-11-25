# Shape-A API Friction Self-Review

This is a design note from the in-tree grid-painter dogfooding consumer. It is
not external feedback and must not be counted as confirmed adoption.

## Findings

- The `MediatedMut<T>` post-image capture path worked cleanly for a fresh
  non-chess component once each cell had an `OpaqueId` binding.
- The demo had to model a local command queue and run one Bevy update to flush
  local replicated mutations before exchanging packets. That is reasonable for
  an ECS-native consumer; the example now uses
  `interop::drain_world_outbound_packets` so tests/examples can drain owned
  transport packets without reaching into the outbox resources directly.
- Reference-vector tests need to call `encode_transaction` directly. That
  surface is clear, but examples should state that `rkyv` little-endian bytes
  are the portable identity under test.
- The default `OpaqueId(u64)` was enough for the demo. `OpaqueId::new` and
  `OpaqueId::get` make examples/tests self-documenting without adding a
  transport-specific identity policy.

## Outcome

No blocking API issue surfaced. The publish-mode example path now shows the
framework loop explicitly: configure identity/participants, register
components, bind stable ids, mutate locally, drain packets, receive packets,
apply canonically, and handle divergence through the embedding framework's
shutdown or disconnect policy.
