use super::*;

const PEER_ID_HANDSHAKE_MAGIC: [u8; 4] = *b"CBSP";
const PEER_ID_HANDSHAKE_VERSION: u16 = 2;
const PEER_ID_HANDSHAKE_HEADER_LEN: usize = 10;
const PEER_ID_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

impl StreamMuxer for SteamLaneMuxer {
    type Substream = LaneSubstream;
    type Error = SteamMuxerError;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<StreamMuxerEvent, Self::Error>> {
        let this = self.get_mut();

        // Tick-based polling (16ms, matching game frame rate)
        if this.tick.poll_tick(cx).is_pending() {
            return Poll::Pending;
        }

        let mut inner = this.inner.lock().expect("muxer lock poisoned");
        if inner.closed {
            return Poll::Ready(Err(SteamMuxerError::ConnectionClosed));
        }

        // Receive batch from Steam
        let messages = {
            let mut conn = inner.connection.lock().expect("muxer lock poisoned");
            match conn.receive_messages(READ_BATCH_SIZE) {
                Ok(msgs) => msgs,
                Err(_) => {
                    drop(conn);
                    inner.closed = true;
                    inner.wake_all();
                    return Poll::Ready(Err(SteamMuxerError::ConnectionClosed));
                }
            }
        };

        for msg in &messages {
            let lane = msg.channel();
            let data = msg.data();

            // Check if this is a control signal. Signals use a magic prefix so
            // normal one-byte protocol/data writes can never be misclassified.
            if data.len() == 3 && data[0] == SIG_MAGIC_0 && data[1] == SIG_MAGIC_1 {
                let signal = data[2];
                match signal {
                    SIG_OPEN => {
                        // Remote opened a new substream on this lane
                        if let std::collections::hash_map::Entry::Vacant(e) =
                            inner.lanes.entry(lane)
                        {
                            e.insert(LaneState::new());
                            // Remove from free pool if present
                            inner.free_lanes.retain(|&l| l != lane);
                            inner.inbound_queue.push_back(lane);
                            if let Some(w) = inner.inbound_waker.take() {
                                w.wake();
                            }
                        }
                        continue;
                    }
                    SIG_FIN => {
                        if let Some(state) = inner.lanes.get_mut(&lane) {
                            state.remote_closed = true;
                            if let Some(w) = state.read_waker.take() {
                                w.wake();
                            }
                            // If both sides closed, release the lane
                            if state.local_closed {
                                inner.release_lane(lane);
                            }
                        }
                        continue;
                    }
                    SIG_RST => {
                        if let Some(state) = inner.lanes.get_mut(&lane) {
                            state.remote_closed = true;
                            state.reset = true;
                            state.read_buf.clear();
                            if let Some(w) = state.read_waker.take() {
                                w.wake();
                            }
                        }
                        continue;
                    }
                    _ => {} // Not a signal, fall through to data handling
                }
            }

            // Data message — route to the lane's read buffer. Steam guarantees
            // reliable ordering per lane, but the batch returned by
            // receive_messages() is not part of that type contract. If data is
            // observed before the OPEN marker in this poll, create the inbound
            // lane rather than dropping the first protocol bytes.
            if let std::collections::hash_map::Entry::Vacant(e) = inner.lanes.entry(lane) {
                e.insert(LaneState::new());
                inner.free_lanes.retain(|&l| l != lane);
                inner.inbound_queue.push_back(lane);
                if let Some(w) = inner.inbound_waker.take() {
                    w.wake();
                }
            }

            if let Some(state) = inner.lanes.get_mut(&lane)
                && !state.remote_closed
                && !state.reset
            {
                state.read_buf.extend(data);
                if let Some(w) = state.read_waker.take() {
                    w.wake();
                }
            }
        }

        // Re-register for next tick
        cx.waker().wake_by_ref();
        Poll::Pending
    }

    fn poll_inbound(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::Substream, Self::Error>> {
        let this = self.get_mut();
        let mut inner = this.inner.lock().expect("muxer lock poisoned");

        if inner.closed {
            return Poll::Ready(Err(SteamMuxerError::ConnectionClosed));
        }

        if let Some(lane) = inner.inbound_queue.pop_front() {
            return Poll::Ready(Ok(LaneSubstream {
                lane,
                inner: this.inner.clone(),
                local_read_buf: VecDeque::new(),
                tick: tokio::time::interval(TICK_INTERVAL),
                closed: false,
            }));
        }

        inner.inbound_waker = Some(cx.waker().clone());
        Poll::Pending
    }

    fn poll_outbound(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<Self::Substream, Self::Error>> {
        let this = self.get_mut();
        let mut inner = this.inner.lock().expect("muxer lock poisoned");

        if inner.closed {
            return Poll::Ready(Err(SteamMuxerError::ConnectionClosed));
        }

        // Get a free lane, growing if needed
        if inner.free_lanes.is_empty() {
            inner.grow_lanes()?;
        }

        let lane = inner
            .free_lanes
            .pop_front()
            .expect("free_lanes non-empty after grow");

        // Insert lane state and send OPEN signal
        inner.lanes.insert(lane, LaneState::new());
        if let Err(e) = inner.send_signal(lane, SIG_OPEN) {
            inner.release_lane(lane);
            return Poll::Ready(Err(e));
        }

        Poll::Ready(Ok(LaneSubstream {
            lane,
            inner: this.inner.clone(),
            local_read_buf: VecDeque::new(),
            tick: tokio::time::interval(TICK_INTERVAL),
            closed: false,
        }))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        let mut inner = this.inner.lock().expect("muxer lock poisoned");

        if inner.closed {
            return Poll::Ready(Ok(()));
        }

        // Send RST to all active substreams
        let active_lanes: Vec<i32> = inner.lanes.keys().copied().collect();
        log::debug!(
            "SteamLaneMuxer: closing muxer with {} active lanes",
            active_lanes.len()
        );
        for lane in active_lanes {
            let _ = inner.send_signal(lane, SIG_RST);
        }

        inner.lanes.clear();
        inner.closed = true;
        inner.wake_all();

        Poll::Ready(Ok(()))
    }
}

// ---------------------------------------------------------------------------
// LaneSubstream
// ---------------------------------------------------------------------------

/// A libp2p substream backed by a single Steam lane.
///
/// Reads and writes go directly through Steam's message API with
/// `RELIABLE_NO_NAGLE` — no custom framing needed.
pub struct LaneSubstream {
    lane: i32,
    inner: Arc<Mutex<MuxerInner>>,
    /// Local read buffer (moved from shared state to minimize lock hold).
    local_read_buf: VecDeque<u8>,
    /// Tick for write backpressure retry.
    tick: tokio::time::Interval,
    /// Whether we've sent FIN/RST.
    closed: bool,
}

impl AsyncRead for LaneSubstream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();

        // 1. Drain local buffer first (no lock needed)
        if !this.local_read_buf.is_empty() {
            let n = copy_from_deque(&mut this.local_read_buf, buf);
            return Poll::Ready(Ok(n));
        }

        // 2. Check shared state
        let mut inner = this.inner.lock().expect("muxer lock poisoned");

        match inner.lanes.get_mut(&this.lane) {
            Some(state) => {
                if state.reset {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "substream reset by remote",
                    )));
                }

                if !state.read_buf.is_empty() {
                    // Swap to local to minimize lock hold
                    std::mem::swap(&mut this.local_read_buf, &mut state.read_buf);
                    drop(inner);
                    let n = copy_from_deque(&mut this.local_read_buf, buf);
                    return Poll::Ready(Ok(n));
                }

                if state.remote_closed {
                    return Poll::Ready(Ok(0)); // EOF
                }

                state.read_waker = Some(cx.waker().clone());
                Poll::Pending
            }
            None => {
                // Lane was cleaned up
                Poll::Ready(Ok(0)) // EOF
            }
        }
    }
}

impl AsyncWrite for LaneSubstream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let inner = this.inner.lock().expect("muxer lock poisoned");

        if inner.closed {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "muxer closed",
            )));
        }

        if let Some(state) = inner.lanes.get(&this.lane)
            && state.reset
        {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "substream reset by remote",
            )));
        }

        match inner.send_on_lane(this.lane, buf, SendFlags::RELIABLE_NO_NAGLE) {
            Ok(()) => Poll::Ready(Ok(buf.len())),
            Err(SteamMuxerError::Backpressure) => {
                log::debug!("SteamLaneMuxer: backpressure on lane {}", this.lane);
                drop(inner);
                let _ = this.tick.poll_tick(cx);
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                e.to_string(),
            ))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Steam handles flushing with NO_NAGLE
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.closed {
            return Poll::Ready(Ok(()));
        }

        let mut inner = this.inner.lock().expect("muxer lock poisoned");
        if !inner.closed {
            let _ = inner.send_signal(this.lane, SIG_FIN);
        }

        if let Some(state) = inner.lanes.get_mut(&this.lane) {
            state.local_closed = true;
            if state.remote_closed {
                inner.release_lane(this.lane);
            }
        }

        this.closed = true;
        Poll::Ready(Ok(()))
    }
}

impl Drop for LaneSubstream {
    fn drop(&mut self) {
        if !self.closed
            && let Ok(mut inner) = self.inner.lock()
        {
            if !inner.closed {
                let _ = inner.send_signal(self.lane, SIG_RST);
            }
            inner.release_lane(self.lane);
        }
    }
}

// ---------------------------------------------------------------------------
// PeerId handshake
// ---------------------------------------------------------------------------

/// Exchange `PeerIds` over a raw `SteamStream` (replaces Noise handshake).
///
/// Both sides send `[magic: 4][version: u16 LE][len: u32 LE][PeerId bytes]`
/// and receive the remote's. Steam Datagram Relay already provides encryption.
pub async fn peer_id_handshake(
    stream: &crate::stream::SteamStream,
    local_peer_id: &libp2p_identity::PeerId,
) -> Result<libp2p_identity::PeerId, SteamMuxerError> {
    // We need a mutable reference — the stream is behind Arc<Mutex<>> internally
    // and we use its AsyncRead/AsyncWrite impls.
    // However, SteamStream's poll_read/poll_write take Pin<&mut Self>.
    // We'll use a scoped approach: clone the shared connection and do raw
    // Steam message sends/receives for the handshake.

    let connection = stream.shared_connection();
    let local_frame = encode_peer_id_handshake_frame(local_peer_id)?;

    // Send our PeerId as a raw Steam message on lane 0 (unconfigured = default single lane)
    {
        let conn = connection.lock().expect("muxer lock poisoned");
        log::debug!(
            "Steam peer id handshake: sending v{} frame ({} bytes)",
            PEER_ID_HANDSHAKE_VERSION,
            local_frame.len()
        );
        conn.send_message(&local_frame, SendFlags::RELIABLE_NO_NAGLE)
            .map_err(|e| SteamMuxerError::Steam(format!("handshake send failed: {e}")))?;
    }

    let remote_bytes = tokio::time::timeout(PEER_ID_HANDSHAKE_TIMEOUT, async {
        loop {
            let msgs = {
                let mut conn = connection.lock().expect("muxer lock poisoned");
                conn.receive_messages(1)
                    .map_err(|_| SteamMuxerError::ConnectionClosed)?
            };

            if let Some(msg) = msgs.into_iter().next() {
                let data = msg.data();
                log::debug!(
                    "Steam peer id handshake: received frame candidate ({} bytes)",
                    data.len()
                );
                break decode_peer_id_handshake_frame(data);
            }

            // No message yet — yield and retry
            tokio::time::sleep(Duration::from_millis(16)).await;
        }
    })
    .await
    .map_err(|_| {
        SteamMuxerError::Steam(format!(
            "handshake timed out after {}s",
            PEER_ID_HANDSHAKE_TIMEOUT.as_secs()
        ))
    })??;

    libp2p_identity::PeerId::from_bytes(&remote_bytes)
        .map_err(|e| SteamMuxerError::Steam(format!("invalid remote PeerId: {e}")))
}

pub(crate) fn encode_peer_id_handshake_frame(
    peer_id: &libp2p_identity::PeerId,
) -> Result<Vec<u8>, SteamMuxerError> {
    let local_bytes = peer_id.to_bytes();
    let len = u32::try_from(local_bytes.len())
        .map_err(|_| SteamMuxerError::Steam("handshake: PeerId too large".into()))?;
    let mut frame = Vec::with_capacity(PEER_ID_HANDSHAKE_HEADER_LEN + local_bytes.len());
    frame.extend_from_slice(&PEER_ID_HANDSHAKE_MAGIC);
    frame.extend_from_slice(&PEER_ID_HANDSHAKE_VERSION.to_le_bytes());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&local_bytes);
    Ok(frame)
}

pub(crate) fn decode_peer_id_handshake_frame(data: &[u8]) -> Result<Vec<u8>, SteamMuxerError> {
    if data.len() < PEER_ID_HANDSHAKE_HEADER_LEN {
        return Err(SteamMuxerError::Steam("handshake: runt frame".into()));
    }
    if data[0..4] != PEER_ID_HANDSHAKE_MAGIC[..] {
        return Err(SteamMuxerError::Steam("handshake: bad magic".into()));
    }
    let version = u16::from_le_bytes([data[4], data[5]]);
    if version != PEER_ID_HANDSHAKE_VERSION {
        return Err(SteamMuxerError::Steam(format!(
            "handshake: unsupported version {version}"
        )));
    }
    let len = u32::from_le_bytes([data[6], data[7], data[8], data[9]]) as usize;
    let expected_len = PEER_ID_HANDSHAKE_HEADER_LEN
        .checked_add(len)
        .ok_or_else(|| SteamMuxerError::Steam("handshake: length overflow".into()))?;
    if data.len() != expected_len {
        return Err(SteamMuxerError::Steam(format!(
            "handshake: length mismatch, expected {expected_len} bytes, got {}",
            data.len()
        )));
    }
    Ok(data[PEER_ID_HANDSHAKE_HEADER_LEN..].to_vec())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Copy bytes from a `VecDeque` into a slice, draining the front.
fn copy_from_deque(deque: &mut VecDeque<u8>, buf: &mut [u8]) -> usize {
    let n = std::cmp::min(deque.len(), buf.len());
    for (i, byte) in deque.drain(..n).enumerate() {
        buf[i] = byte;
    }
    n
}
