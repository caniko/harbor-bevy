# libp2p-steam-transport — Implementation Guide

A libp2p `Transport` over Valve's Steam Networking Sockets (`ISteamNetworkingSockets`).
Replaces Noise + Yamux with Steam-native encryption and lane-based multiplexing.

---

## What This Crate Does

A standard libp2p `Transport` carries peer-to-peer connections over Steam
**Datagram Relay** (SDR) without changes to request-response, piying remote
actors, or other libp2p protocols.

- SDR already encrypts traffic; Noise would encrypt it twice for no benefit.
- `ConfigureConnectionLanes` provides independent, reliable, ordered delivery
  per lane. One lane per substream replaces Yamux without framing overhead.

```
Before:  SteamTransport → Noise → Yamux → libp2p protocols
After:   SteamTransport → PeerId handshake → SteamLaneMuxer → libp2p protocols
```

---

## How a Connection is Established

### Listening (accepting connections)

```
listen_on("/memory/<our_steam_id>")
    │
    ▼
create_listen_socket_p2p(virtual_port)
    │
    ▼
spawn_blocking thread                           ← listener.rs
    blocks on ListenSocket::events()            ← 0 CPU while idle
    │
    ├─ Connecting → request.accept()
    ├─ Connected  → send NetConnection via mpsc
    └─ Disconnected → log
    │
    ▼
Transport::poll() receives the NetConnection
    │
    ▼
TransportEvent::Incoming { upgrade: SteamStream }
```

The Bevy main thread calls `Client::run_callbacks()` to deliver Steam events.
The `spawn_blocking` bridge forwards accepted connections to libp2p's async
`poll()` and uses no CPU while blocked on `std::sync::mpsc::recv()`.

### Dialing (outbound connections)

```
dial("/memory/<remote_steam_id>")
    │
    ▼
connect_p2p(NetworkingIdentity(remote_id), virtual_port)
    │
    ▼
spawn_blocking thread                           ← dial.rs
    polls get_connection_info() every 16ms
    │
    ├─ Connected → send Ok(NetConnection) via oneshot
    ├─ ClosedByPeer/ProblemDetected → send Err
    └─ Still connecting → sleep 16ms, retry
    │
    ▼
dial future resolves → SteamStream
```

The steamworks crate doesn't implement the independent connection callback.
Polling every 16ms aligns with a 60Hz game tick; a typical P2P dial task lasts
about 1 second.

### After connection: handshake + muxer setup

The swarm integration layer uses `Transport::and_then()` to add identity
exchange and multiplexing to the raw `SteamStream`:

```
SteamStream (raw byte stream on lane 0)
    │
    ▼
peer_id_handshake()                             ← muxer.rs
    Both sides send:  [u32 LE length][PeerId bytes]
    Both sides recv:  [u32 LE length][PeerId bytes]
    │
    ▼
SteamLaneMuxer::from_stream()                   ← muxer.rs
    Calls ConfigureConnectionLanes (8 lanes)
    All lanes go into the free pool
    │
    ▼
StreamMuxerBox(muxer)  → libp2p swarm
```

---

## Lane Muxer: How Substreams Work

Each libp2p substream gets a Steam lane with reliable, ordered delivery and no
frame headers or stream IDs.

### Opening a substream (outbound)

1. Pop a lane from the free pool (grow if empty)
2. Send a 1-byte `OPEN` (0x01) signal on that lane
3. Return a `LaneSubstream` backed by that lane

### Receiving a substream (inbound)

1. `poll()` reads messages from Steam via `receive_messages(64)`
2. Spots a 1-byte `OPEN` signal on lane N
3. Creates a `LaneState` for that lane, enqueues it
4. `poll_inbound()` returns a `LaneSubstream` for lane N

### Reading and writing

- **Write**: `send_messages()` tags the data with the lane's channel number and
  uses `RELIABLE_NO_NAGLE`. On backpressure (full Steam buffer), the write returns
  `Pending` and retries on the next 16ms tick.
- **Read**: The muxer's `poll()` routes incoming messages by `msg.channel()` to
  each lane's `VecDeque<u8>`. Reads check that buffer and swap it out from under
  the lock to reduce contention.

### Closing

| Signal | Byte | Meaning |
|--------|------|---------|
| OPEN   | 0x01 | New substream on this lane (or lane reused) |
| FIN    | 0x02 | Graceful write-side close |
| RST    | 0x04 | Abortive close |

Signals are standalone 1-byte Steam messages, never mixed into data.

- **Graceful close** (`AsyncWrite::poll_close`): sends FIN, marks `local_closed`.
  When both sides have closed (local FIN sent + remote FIN received), the lane
  returns to the free pool.
- **Drop without close**: sends RST, returns lane to pool immediately.
- **Muxer close** (`StreamMuxer::poll_close`): sends RST on all active lanes.

### Lane pool

- Starts at 8 lanes (Steam says "3 or so is fine; >8 is a lot")
- All lanes have equal priority and weight (ordering between substreams doesn't matter)
- When the pool is empty, `grow_lanes()` calls `ConfigureConnectionLanes` to add
  4 more. Steam allows increasing the lane count, but not decreasing.
- For a 1v1 game: typically 3–5 concurrent substreams (request-response + piying
  remote messaging). 8 lanes is plenty.

---

## File Map

```
src/
├── lib.rs          Public API surface: module declarations and re-exports
├── transport.rs    SteamTransport — implements libp2p_core::Transport
├── stream.rs       SteamStream — AsyncRead/AsyncWrite byte stream
├── muxer.rs        SteamLaneMuxer — StreamMuxer (one lane per substream)
├── lane.rs         LaneConfig and shared lane configuration
├── protocol.rs     /memory/<steam_id> multiaddr encoding
├── error.rs        SteamTransportError
├── listener.rs     spawn_blocking bridge for listen socket events
├── dial.rs         spawn_blocking poller for dial connections
└── tests.rs        Unit tests (no Steam required)

tests/
└── steam_loopback.rs   Integration tests (require Steam running)
```

### transport.rs — The Transport Layer

- `SteamTransportConfig` — the socket handle, virtual ports, and local Steam ID
- `SteamTransport` — the entry point; manages listeners and dial futures and
  implements `libp2p_core::Transport` with `Output = SteamStream`

Polling drains queued events (NewAddress, ListenerClosed), then polls each
listener's mpsc receiver for incoming connections and removes listeners whose
bridges have exited.

### stream.rs — The Byte Stream

A bidirectional byte stream over a single `NetConnection`.

- Reads call `receive_messages()` directly. If empty, return `Pending` and retry in 16ms.
- Writes call `send_message()` synchronously. A full buffer signals backpressure
  with `Pending`.
- `Arc<Mutex<>>` lets the stream share the `NetConnection` with the muxer.

### muxer.rs — The Heart of the Crate

The Yamux replacement (~710 lines):
- `SteamLaneMuxer` — owns `Arc<Mutex<MuxerInner>>` + a 16ms tick interval
- `MuxerInner` — connection, per-lane state (`HashMap<i32, LaneState>`), free pool,
  inbound queue
- `LaneSubstream` — a single substream backed by one lane; implements `AsyncRead` +
  `AsyncWrite`; sends RST on drop if not cleanly closed
- `LaneState` — per-lane buffer, waker, closed/reset flags
- `SteamMuxerError` — ConnectionClosed, Backpressure, Steam(String), Io(Error)

`peer_id_handshake()` exchanges PeerIds in raw Steam messages before lane setup.
Both peers send `[u32 LE length][PeerId bytes]` and poll every 16ms for the
other peer's message.

### lane.rs — Lane Configuration

- `LaneConfig` — number of lanes, per-lane priorities, per-lane bandwidth weights
- `configure_lanes()` — calls `sockets.configure_connection_lanes()`

### protocol.rs — Addressing

The `/memory/<steam_id>` multiaddr reuses libp2p's `Protocol::Memory(u64)` for
`u64` Steam IDs:
- `steam_multiaddr(steam_id) -> Multiaddr`
- `parse_steam_id(addr) -> Option<u64>`

### error.rs — Transport-Level Errors

Four variants: `ListenFailed`, `ConnectFailed(String)`, `DialCancelled`, `Steam(String)`.

### listener.rs / dial.rs — Async Bridges

Both use `tokio::task::spawn_blocking` to bridge Steam's synchronous callbacks
to tokio's async runtime.

- **listener.rs**: Blocks on `ListenSocket::events()`. Forwards accepted
  connections via `tokio::sync::mpsc`. The bridge exits when the receiver is
  dropped, including on listener removal.
- **dial.rs**: Polls `get_connection_info()` every 16ms until the connection reaches
  `Connected`, fails, or the receiver is dropped (dial cancelled).

---

## Threading Model

```
┌─────────────────────────────────────────────────────────┐
│  Bevy Main Thread                                       │
│    Client::run_callbacks()  ← fires Steam callbacks     │
└──────────┬──────────────────────────────────────────────┘
           │ callbacks trigger ListenSocket::events()
           ▼
┌─────────────────────────────────────────────────────────┐
│  spawn_blocking: listener bridge                        │
│    blocks on ListenSocket::events() (0 CPU idle)        │
│    sends NetConnection via tokio::sync::mpsc            │
└──────────┬──────────────────────────────────────────────┘
           │
           ▼
┌─────────────────────────────────────────────────────────┐
│  Swarm thread (tokio single-thread runtime)            │
│    drives libp2p Swarm                                  │
│    SteamLaneMuxer::poll() reads messages                │
│    LaneSubstream read/write via Arc<Mutex<MuxerInner>>  │
└─────────────────────────────────────────────────────────┘
```

**Shared state:**
- `Arc<Mutex<NetConnection>>` — shared between SteamStream and SteamLaneMuxer
- `Arc<Mutex<MuxerInner>>` — shared between SteamLaneMuxer and all LaneSubstreams
- Crossbeam channels for lane message forwarding

---

## Constants

| Name | Value | Why |
|------|-------|-----|
| `DEFAULT_INITIAL_LANES` | 8 | Steam recommends ≤8 lanes; enough for typical game traffic |
| `LANE_GROW_STEP` | 4 | Added when pool exhausts; conservative growth |
| `READ_BATCH_SIZE` | 64 | Messages per `receive_messages()` call |
| `TICK_INTERVAL` | 16ms | Aligns with 60Hz game tick for polling cadence |
| `SIG_OPEN` | 0x01 | Lane open signal |
| `SIG_FIN` | 0x02 | Graceful close signal |
| `SIG_RST` | 0x04 | Abortive close signal |

---

## Public API

```rust
// Transport
pub struct SteamTransport { .. }
pub struct SteamTransportConfig {
    pub networking_sockets: NetworkingSockets,
    pub virtual_port: i32,
    pub local_steam_id: u64,
    pub dial_virtual_port: Option<i32>,
}

// Muxer
pub struct SteamLaneMuxer { .. }   // implements StreamMuxer
pub struct LaneSubstream { .. }     // implements AsyncRead + AsyncWrite
pub enum SteamMuxerError { .. }
pub async fn peer_id_handshake(stream, local_peer_id) -> Result<PeerId>;

// Stream
pub struct SteamStream { .. }       // implements AsyncRead + AsyncWrite

// Lanes
pub struct LaneConfig { pub num_lanes, priorities, weights }

// Addressing
pub fn steam_multiaddr(steam_id: u64) -> Multiaddr;
pub fn parse_steam_id(addr: &Multiaddr) -> Option<u64>;

// Errors
pub enum SteamTransportError { .. }

// Re-exports from steamworks (so consumers don't need a direct dep)
pub use steamworks::networking_sockets::NetworkingSockets;
pub use steamworks::networking_utils::NetworkingUtils;
```

---

## Integration with Thespan

The [Thespan Steam transport](../thespan/src/transport/steam.rs) exchanges peer
IDs, then runs Yamux on the reliable Steam stream. Piying remote actors run as
a libp2p `NetworkBehaviour` independent of the transport.

---

## Testing

### Unit tests (no Steam required)

```sh
cargo test -p libp2p-steam-transport
```

Covers: `copy_from_deque` edge cases, multiaddr validation (listen/dial address
parsing, rejection of TCP addresses, wildcard rejection).

### Integration tests (require Steam running)

```sh
cargo test -p libp2p-steam-transport --test steam_loopback -- --ignored
```

CI skips these `#[ignore]` tests. Run the Steam client with App ID 480
(Valve's Spacewar test app; no purchase needed).

Covers:
- Transport creation
- Listen on/remove
- Dial (returns future, rejects wildcard, rejects TCP)
- Virtual port override for loopback
- Raw `ISteamNetworkingSockets` P2P loopback (validates SDK works before
  involving the full libp2p stack)

---

## Design Decisions

### Why no Noise?

Steam Datagram Relay encrypts all traffic at the transport level. Noise would
encrypt it twice, adding measurable latency but no security benefit. A PeerId
exchange replaces Noise's handshake for peer identity.

### Why one lane per substream instead of custom framing?

A `RELIABLE` Steam lane already provides the independently ordered byte stream
that `AsyncRead + AsyncWrite` needs. Multiplexing streams on one lane (as Yamux
would) adds frame headers, parsing, and head-of-line blocking between streams.
A 1:1 mapping sends bytes straight to Steam without a wrapper.

### Why 16ms tick (not 1ms)?

Interactive applications trade up to 16ms of extra latency for fewer wake-ups.

### Why spawn_blocking bridges?

The Steam SDK delivers callbacks on the thread that calls `run_callbacks()`.
`spawn_blocking` threads wait on Steam's event iterators with 0 CPU while idle
and bridge to libp2p's `Future`-based async API.

### Why `/memory/<steam_id>` multiaddrs?

Steam IDs are `u64`. The libp2p multiaddr spec's `Protocol::Memory(u64)` is
unused for real networking, so no custom protocol registration is needed.

---

## Dependencies

| Crate | Version | What it's for |
|-------|---------|---------------|
| `libp2p-core` | 0.43 | Transport trait, StreamMuxer trait, Multiaddr |
| `libp2p-identity` | 0.2 | PeerId type for handshake |
| `steamworks` | 0.13 | ISteamNetworkingSockets bindings |
| `tokio` | 1 | Async runtime (sync, time, rt features) |
| `futures` | 0.3 | AsyncRead/AsyncWrite traits |
| `log` | 0.4 | Structured logging |
