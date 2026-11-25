# Dogfooding Release Rationale

`bevy_replicated` is in in-tree development mode. There is no crates.io artifact
for an outside project to adopt, so confirmed external adoption is currently
impossible.

The `0.1.x` line is therefore framed as a dogfooding release:

- Exercised by the networked Bevy game from which the substrate was extracted.
- Demonstrated in a second, non-chess domain by the
  `bevy-replicated-grid-painter` workspace crate under
  `crates/examples/replicated-grid-painter`.
- Not yet externally adopted, and public docs must not imply otherwise.

The grid-painter demo depends on the in-tree `bevy_replicated` path dependency,
uses `OpaqueId(u64)` identities, drives two in-process peers through
`MediatedMut<T>` post-image capture, exchanges packets through the public
`interop` drain/receive helpers, and checks that `ReplicationChain` hashes
converge.

The general-purpose claim should be phrased as:

> Designed as a general-purpose replication substrate and demonstrated by an
> in-tree non-chess consumer; externally validated as of pending.

The graduation trigger is the first confirmed external consumer: a project that
depends on a published crates.io `bevy_replicated`, runs its own tests green
against that version, and files at least one issue, PR, or written API critique
against the crate.
