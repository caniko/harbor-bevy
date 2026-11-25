# Vendored libp2p patches

The stock workspace uses two patched crates.io snapshots through its root
`[patch.crates-io]`. They use the registry `libp2p-core`, keeping trait identities
shared with the rest of the libp2p stack.

The root also patches `libp2p-dns` and `libp2p-mdns` to the disabled crates
under `crates/net`. DNS is unused; piying enables mDNS transitively but does
not use it with the rkyv codec. Both shadows keep the resolver dependencies
out of the lockfile; they do not provide working DNS or mDNS behaviours.

| Snapshot | Patch |
| --- | --- |
| `libp2p-swarm-0.47.1` | Skip stale substream requests instead of extracting one twice and panicking. [rust-libp2p#6762](https://github.com/libp2p/rust-libp2p/pull/6762) is the upstream fix candidate. |
| `libp2p-relay-0.21.1` | Send the outbound circuit negotiation to the destination peer's connection handler. [rust-libp2p#6763](https://github.com/libp2p/rust-libp2p/pull/6763). |

Retire the snapshots and their patch entries once a published libp2p release
contains both fixes and libp2p, piying, WebRTC, and Steam transport can move to
that release family together. Recheck the DNS/mDNS shadow patch versions if
their upstream packages change. Before removal, verify that the resolved graph
has one `libp2p-core` and one `libp2p-swarm`, and run the direct-connection,
concurrent-substream, and relay regressions against the unpatched release.
