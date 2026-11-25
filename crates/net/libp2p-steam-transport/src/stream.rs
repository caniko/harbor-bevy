use futures::io::{AsyncRead, AsyncWrite};
use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use steamworks::SteamError;
use steamworks::networking_sockets::NetConnection;
use steamworks::networking_types::SendFlags;

use crate::{READ_BATCH_SIZE, TICK_INTERVAL};

/// A bidirectional byte stream over a Steam `NetConnection`.
///
/// - **Reads** call `receive_messages()` directly on the connection. When no
///   data is available, the stream returns `Pending` and re-polls after a 16ms
///   tick (aligned with ~60Hz game tick rate).
///
/// - **Writes** call `send_message()` synchronously. If Steam's internal send
///   buffer is full (`LimitExceeded`), the write returns `Pending` and retries
///   on the next 16ms tick (backpressure).
///
/// The `NetConnection` is wrapped in `Arc<Mutex<>>` so the stream can be
/// upgraded into the lane-backed muxer. Dropping all holders closes the
/// connection via Steam SDK.
pub struct SteamStream {
    connection: Arc<Mutex<NetConnection>>,
    read_buf: VecDeque<u8>,
    tick: tokio::time::Interval,
}

impl SteamStream {
    /// Create a stream without lane support.
    pub fn new(connection: NetConnection) -> Self {
        Self {
            connection: Arc::new(Mutex::new(connection)),
            read_buf: VecDeque::new(),
            tick: tokio::time::interval(TICK_INTERVAL),
        }
    }

    /// Get a shared reference to the underlying connection.
    pub fn shared_connection(&self) -> Arc<Mutex<NetConnection>> {
        self.connection.clone()
    }
}

impl AsyncRead for SteamStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        // 1. Drain buffered data first
        if !self.read_buf.is_empty() {
            let n = copy_from_deque(&mut self.read_buf, buf);
            return Poll::Ready(Ok(n));
        }

        // 2. Try to receive from Steam
        let mut conn = self.connection.lock().expect("stream lock poisoned");
        match conn.receive_messages(READ_BATCH_SIZE) {
            Ok(messages) if !messages.is_empty() => {
                drop(conn); // Release lock before processing
                for msg in &messages {
                    self.read_buf.extend(msg.data());
                }
                let n = copy_from_deque(&mut self.read_buf, buf);
                Poll::Ready(Ok(n))
            }
            Ok(_) => {
                drop(conn);
                // No data available — wait for next tick
                if self.tick.poll_tick(cx).is_ready() {
                    cx.waker().wake_by_ref();
                }
                Poll::Pending
            }
            Err(_) => {
                drop(conn);
                // InvalidHandle — connection is dead
                Poll::Ready(Ok(0)) // EOF
            }
        }
    }
}

impl AsyncWrite for SteamStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let conn = this.connection.lock().expect("stream lock poisoned");
        match conn.send_message(buf, SendFlags::RELIABLE_NO_NAGLE) {
            Ok(_) => Poll::Ready(Ok(buf.len())),
            Err(SteamError::LimitExceeded) => {
                drop(conn);
                // Send buffer full — backpressure, retry on next tick
                if this.tick.poll_tick(cx).is_ready() {
                    cx.waker().wake_by_ref();
                }
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                format!("Steam send failed: {e}"),
            ))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Steam handles flushing internally with NO_NAGLE flag
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // NetConnection::drop() handles closing via Steam SDK
        Poll::Ready(Ok(()))
    }
}

/// Copy bytes from a `VecDeque` into a slice, draining the front.
pub(crate) fn copy_from_deque(deque: &mut VecDeque<u8>, buf: &mut [u8]) -> usize {
    let n = std::cmp::min(deque.len(), buf.len());
    for (i, byte) in deque.drain(..n).enumerate() {
        buf[i] = byte;
    }
    n
}
