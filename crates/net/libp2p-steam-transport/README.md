# libp2p-steam-transport

[![crates.io](https://img.shields.io/crates/v/libp2p-steam-transport.svg)](https://crates.io/crates/libp2p-steam-transport)
[![docs.rs](https://docs.rs/libp2p-steam-transport/badge.svg)](https://docs.rs/libp2p-steam-transport)

A [libp2p](https://libp2p.io/) `Transport` over
[Steam Networking Sockets](https://partner.steamgames.com/doc/api/ISteamNetworkingSockets)
(`ISteamNetworkingSockets`) for peer-to-peer connections over Valve's Steam relay.

## Overview

`SteamTransport` implements `libp2p_core::Transport` and composes with Noise
for PeerId-based identity and Yamux for substream multiplexing.

### Architecture

- **Listen path** &mdash; A `spawn_blocking` bridge blocks on
  `ListenSocket::events()` (0 CPU while idle), forwarding accepted connections
  via `tokio::sync::mpsc`.
- **Dial path** &mdash; A `spawn_blocking` task polls `get_connection_info()`
  every 16 ms until the connection succeeds or fails.
- **Stream I/O** &mdash; 16 ms read-polling tick, synchronous writes with
  `RELIABLE_NO_NAGLE` and backpressure on `LimitExceeded`.
- **Addressing** &mdash; `/memory/<steam_id>` reuses `Protocol::Memory(u64)`
  from the multiaddr spec.

## Usage

```rust
use libp2p_steam_transport::{SteamTransport, SteamTransportConfig};

let transport = SteamTransport::new(SteamTransportConfig {
    networking_sockets: client.networking_sockets(),
    virtual_port: 0,
    local_steam_id: client.user().steam_id().raw(),
});

// Compose with Noise + Yamux as usual:
let transport = transport
    .upgrade(libp2p_core::upgrade::Version::V1Lazy)
    .authenticate(libp2p_noise::Config::new(&keypair).unwrap())
    .multiplex(libp2p_yamux::Config::default())
    .boxed();
```

## Compatibility

| Crate | Version |
|-------|---------|
| `libp2p-core` | 0.43.x |
| `steamworks` | 0.13.x |
| `tokio` | 1.x |

Requires Rust 1.85+ (edition 2024).

## Steam Networking: Terms of Service

Using Valve's Steam Networking relay (`ISteamNetworkingSockets`) means
agreeing to the
[Steam API Terms of Use](https://partner.steamgames.com/doc/sdk/api_terms).

- Steam relay access requires a valid **Steamworks partner account** and a
  published (or in-development) Steam application.
- Relay traffic crosses Valve's global network; Valve's
  [privacy policy](https://store.steampowered.com/privacy_agreement/) applies
  to data transiting their infrastructure.
- The relay costs Steamworks partners nothing extra, but Valve reserves the
  right to rate-limit or revoke access for abuse.

Steamworks documentation:

- [ISteamNetworkingSockets](https://partner.steamgames.com/doc/api/ISteamNetworkingSockets)
- [Steam Networking overview](https://partner.steamgames.com/doc/features/multiplayer/networking)
- [Steam API Terms of Use](https://partner.steamgames.com/doc/sdk/api_terms)

## License

Licensed under either of:

- Apache License, Version 2.0
  ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT License
  ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you, as defined in the Apache-2.0 license, shall
be dual-licensed as above, without any additional terms or conditions.
