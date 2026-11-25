//! Disabled `libp2p-mdns` patch.
//!
//! Piying enables libp2p's `mdns` feature transitively, but the rkyv remote
//! actor stack does not use it. This shadow excludes the resolver dependency
//! graph. It is not compatible with Piying's serde-only mDNS bootstrap API.
