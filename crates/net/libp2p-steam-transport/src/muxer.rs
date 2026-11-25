//! Lane-aware `StreamMuxer` that maps each libp2p substream to its own Steam lane.
//!
//! Each Steam lane with `RELIABLE` delivery is an independently ordered byte stream,
//! which is exactly what `StreamMuxer::Substream` requires. Instead of multiplexing
//! N substreams within 1 lane using framing (like Yamux), each substream gets its own
//! lane. No frame headers, no stream IDs — Steam's native per-lane ordering replaces
//! Yamux entirely.
//!
//! # Lane Pool
//!
//! Lanes are assigned from a free pool. When a substream closes, its lane returns to
//! the pool. If the pool exhausts, `ConfigureConnectionLanes` grows the lane count
//! (Steam allows increasing, not decreasing).
//!
//! # Signaling
//!
//! Minimal 1-byte control messages per lane:
//! - `OPEN` (0x01): First byte on a (re)used lane — remote creates inbound substream
//! - `FIN`  (0x02): Graceful write-side close
//! - `RST`  (0x04): Abortive close
//!
//! These are sent as standalone Steam messages, never mixed into data payloads.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use futures::io::{AsyncRead, AsyncWrite};
use libp2p_core::muxing::{StreamMuxer, StreamMuxerEvent};
use steamworks::networking_sockets::{NetConnection, NetworkingSockets};
use steamworks::networking_types::SendFlags;
use steamworks::networking_utils::NetworkingUtils;

use crate::lane::{self, LaneConfig};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Signal: new substream opened on this lane (or lane reused).
pub(crate) const SIG_OPEN: u8 = 0x01;
/// Signal: graceful write-side close.
pub(crate) const SIG_FIN: u8 = 0x02;
/// Signal: abortive close.
pub(crate) const SIG_RST: u8 = 0x04;
pub(crate) const SIG_MAGIC_0: u8 = 0xCB;
pub(crate) const SIG_MAGIC_1: u8 = 0x5E;

use crate::{READ_BATCH_SIZE, TICK_INTERVAL};

/// How many lanes to add when the pool is exhausted.
const LANE_GROW_STEP: i32 = 4;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum SteamMuxerError {
    /// The Steam connection is dead.
    ConnectionClosed,
    /// Steam's send buffer is full.
    Backpressure,
    /// Generic Steam SDK error.
    Steam(String),
    /// IO wrapper.
    Io(io::Error),
}

impl fmt::Display for SteamMuxerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectionClosed => write!(f, "connection closed"),
            Self::Backpressure => write!(f, "send buffer full"),
            Self::Steam(msg) => write!(f, "steam error: {msg}"),
            Self::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for SteamMuxerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for SteamMuxerError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

// ---------------------------------------------------------------------------
// Per-lane substream state
// ---------------------------------------------------------------------------

struct LaneState {
    /// Buffered received data for this substream.
    read_buf: VecDeque<u8>,
    /// Waker registered by the substream's `poll_read`.
    read_waker: Option<Waker>,
    /// Remote sent FIN or RST.
    remote_closed: bool,
    /// We sent FIN or RST.
    local_closed: bool,
    /// RST received — reads return `ConnectionReset`.
    reset: bool,
}

impl LaneState {
    fn new() -> Self {
        Self {
            read_buf: VecDeque::new(),
            read_waker: None,
            remote_closed: false,
            local_closed: false,
            reset: false,
        }
    }
}

// ---------------------------------------------------------------------------
// MuxerInner (shared between muxer and substreams)
// ---------------------------------------------------------------------------

struct MuxerInner {
    connection: Arc<Mutex<NetConnection>>,
    sockets: Arc<NetworkingSockets>,
    utils: Arc<NetworkingUtils>,

    /// Per-lane substream state, keyed by lane index.
    lanes: HashMap<i32, LaneState>,

    /// Free lane pool (available for new substreams).
    /// Only contains lanes matching our `parity` (even for initiator, odd for responder).
    free_lanes: VecDeque<i32>,

    /// Total number of configured lanes.
    total_lanes: i32,

    /// Lane parity: 0 for initiator (uses even lanes), 1 for responder (uses odd lanes).
    /// Prevents lane ID collisions when both sides open substreams simultaneously
    /// (same idea as Yamux's odd/even stream IDs).
    parity: i32,

    /// Lanes opened by remote, not yet yielded via `poll_inbound`.
    inbound_queue: VecDeque<i32>,
    /// Waker for `poll_inbound` when queue is empty.
    inbound_waker: Option<Waker>,

    /// True after connection dies or `poll_close` completes.
    closed: bool,
}

impl MuxerInner {
    /// Send a raw message on a specific lane.
    fn send_on_lane(
        &self,
        lane: i32,
        data: &[u8],
        flags: SendFlags,
    ) -> Result<(), SteamMuxerError> {
        if self.closed {
            return Err(SteamMuxerError::ConnectionClosed);
        }
        let mut msg = self.utils.allocate_message(0);
        {
            let conn = self.connection.lock().expect("muxer lock poisoned");
            msg.set_connection(&conn);
        }
        msg.set_send_flags(flags);
        msg.set_channel(lane);
        msg.set_data(data.to_vec())
            .map_err(|e| SteamMuxerError::Steam(format!("set_data failed: {e:?}")))?;

        let results = self.sockets.send_messages(vec![msg]);
        for result in results {
            if let Err(e) = result {
                let s = format!("{e}");
                if s.contains("LimitExceeded") {
                    return Err(SteamMuxerError::Backpressure);
                }
                return Err(SteamMuxerError::Steam(format!("send_messages failed: {e}")));
            }
        }
        Ok(())
    }

    /// Send a 1-byte signal on a lane.
    fn send_signal(&self, lane: i32, signal: u8) -> Result<(), SteamMuxerError> {
        self.send_on_lane(
            lane,
            &[SIG_MAGIC_0, SIG_MAGIC_1, signal],
            SendFlags::RELIABLE_NO_NAGLE,
        )
    }

    /// Grow the lane pool by `LANE_GROW_STEP * 2` total lanes.
    ///
    /// Both sides need to grow symmetrically (each side adds `LANE_GROW_STEP`
    /// lanes matching its parity).
    fn grow_lanes(&mut self) -> Result<(), SteamMuxerError> {
        // Grow by enough to guarantee at least LANE_GROW_STEP new lanes for our parity.
        let new_total = self.total_lanes + LANE_GROW_STEP * 2;
        let priorities = vec![0i32; new_total as usize];
        let weights = vec![1u16; new_total as usize];

        {
            let conn = self.connection.lock().expect("muxer lock poisoned");
            self.sockets
                .configure_connection_lanes(&conn, new_total, &priorities, &weights)
                .map_err(|e| {
                    SteamMuxerError::Steam(format!("configure_connection_lanes failed: {e}"))
                })?;
        }

        // Only add lanes matching our parity to the free pool
        for i in self.total_lanes..new_total {
            if i % 2 == self.parity {
                self.free_lanes.push_back(i);
            }
        }
        self.total_lanes = new_total;

        log::debug!(
            "SteamLaneMuxer: grew to {new_total} lanes ({} free for parity {})",
            self.free_lanes.len(),
            self.parity,
        );
        Ok(())
    }

    /// Return a lane to the free pool.
    fn release_lane(&mut self, lane: i32) {
        self.lanes.remove(&lane);
        self.free_lanes.push_back(lane);
    }

    /// Wake all substream readers (used on connection death).
    fn wake_all(&mut self) {
        for state in self.lanes.values_mut() {
            if let Some(w) = state.read_waker.take() {
                w.wake();
            }
        }
        if let Some(w) = self.inbound_waker.take() {
            w.wake();
        }
    }
}

// ---------------------------------------------------------------------------
// SteamLaneMuxer
// ---------------------------------------------------------------------------

/// Lane-aware `StreamMuxer` for Steam connections.
///
/// Each libp2p substream is backed by its own Steam lane. Reliable ordered
/// delivery per lane satisfies the `AsyncRead + AsyncWrite` contract without
/// any custom framing.
pub struct SteamLaneMuxer {
    inner: Arc<Mutex<MuxerInner>>,
    tick: tokio::time::Interval,
}

impl SteamLaneMuxer {
    /// Create a muxer from a `SteamStream`.
    ///
    /// Configures `initial_lanes` on the connection and populates the free pool.
    /// The `SteamStream` is consumed — its `NetConnection` is extracted for
    /// direct message-oriented access.
    ///
    /// `is_initiator` determines lane parity to prevent collisions when both
    /// sides open substreams simultaneously (like Yamux's odd/even stream IDs):
    /// - `true` (dialer): uses even-numbered lanes (0, 2, 4, …)
    /// - `false` (listener): uses odd-numbered lanes (1, 3, 5, …)
    pub fn from_stream(
        stream: crate::stream::SteamStream,
        sockets: Arc<NetworkingSockets>,
        utils: Arc<NetworkingUtils>,
        initial_lanes: i32,
        is_initiator: bool,
    ) -> Result<Self, SteamMuxerError> {
        let connection = stream.shared_connection();

        // Configure lanes on the connection
        let config = LaneConfig {
            num_lanes: initial_lanes,
            priorities: vec![0; initial_lanes as usize],
            weights: vec![1; initial_lanes as usize],
        };
        {
            let conn = connection.lock().expect("muxer lock poisoned");
            lane::configure_lanes(&sockets, &conn, &config)
                .map_err(|e| SteamMuxerError::Steam(format!("{e}")))?;
        }

        // Split lanes by parity: initiator gets even, responder gets odd.
        let parity = if is_initiator { 0 } else { 1 };
        let free_lanes: VecDeque<i32> = (0..initial_lanes).filter(|i| i % 2 == parity).collect();

        let inner = MuxerInner {
            connection,
            sockets,
            utils,
            lanes: HashMap::new(),
            free_lanes,
            total_lanes: initial_lanes,
            parity,
            inbound_queue: VecDeque::new(),
            inbound_waker: None,
            closed: false,
        };

        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
            tick: tokio::time::interval(TICK_INTERVAL),
        })
    }
}

mod stream_muxer;

pub use stream_muxer::{LaneSubstream, peer_id_handshake};
#[cfg(test)]
pub(crate) use stream_muxer::{decode_peer_id_handshake_frame, encode_peer_id_handshake_frame};
