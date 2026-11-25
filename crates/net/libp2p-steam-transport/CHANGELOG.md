# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-02-22

### Added

- Initial release.
- `SteamTransport` implementing `libp2p_core::Transport` over
  `ISteamNetworkingSockets`.
- `SteamStream` implementing `futures::io::{AsyncRead, AsyncWrite}`.
- `spawn_blocking` bridges for callback-to-async conversion (listener and dial).
- `/memory/<steam_id>` multiaddr encoding via `steam_multiaddr()` and
  `parse_steam_id()`.
- Backpressure-aware writes with `RELIABLE_NO_NAGLE`.
- Re-export of `steamworks::networking_sockets::NetworkingSockets`.
