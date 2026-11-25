use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use steamworks::networking_sockets::NetConnection;
use steamworks::networking_types::ListenSocketEvent;
use tokio::sync::mpsc;

const LISTENER_SHUTDOWN_POLL: Duration = Duration::from_millis(16);

pub(crate) struct SteamListener {
    connections: mpsc::UnboundedReceiver<NetConnection>,
    shutdown: std::sync::mpsc::Sender<()>,
}

impl SteamListener {
    pub(crate) fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<NetConnection>> {
        Pin::new(&mut self.connections).poll_recv(cx)
    }

    pub(crate) fn shutdown(&self) {
        let _ = self.shutdown.send(());
    }
}

impl Drop for SteamListener {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Spawn a blocking bridge that reads from a `ListenSocket`'s callback-driven
/// event channel and forwards accepted connections to a tokio mpsc channel.
///
/// The task polls Steam events at the game tick cadence so listener shutdown can
/// close the underlying Steam listen socket even when no further peer events
/// arrive.
pub(crate) fn spawn_listener_bridge(
    listen_socket: steamworks::networking_sockets::ListenSocket,
) -> SteamListener {
    let (tx, rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();

    tokio::task::spawn_blocking(move || {
        run_listener_shutdown_loop(
            || {
                while let Some(event) = listen_socket.try_receive_event() {
                    match event {
                        ListenSocketEvent::Connecting(request) => {
                            if let Err(e) = request.accept() {
                                log::warn!("Failed to accept Steam connection: {e}");
                            }
                        }
                        ListenSocketEvent::Connected(connected) => {
                            let connection = connected.take_connection();
                            if tx.send(connection).is_err() {
                                log::debug!("Listener receiver dropped, exiting bridge");
                                return false;
                            }
                        }
                        ListenSocketEvent::Disconnected(disconnected) => {
                            log::debug!(
                                "Steam listener: peer disconnected (remote: {:?})",
                                disconnected.remote()
                            );
                        }
                    }
                }

                true
            },
            shutdown_rx,
        );

        log::debug!("Steam listen socket closed, bridge exiting");
    });

    SteamListener {
        connections: rx,
        shutdown: shutdown_tx,
    }
}

fn run_listener_shutdown_loop(
    mut drain_events: impl FnMut() -> bool,
    shutdown_rx: std::sync::mpsc::Receiver<()>,
) {
    loop {
        if !drain_events() {
            return;
        }

        match shutdown_rx.recv_timeout(LISTENER_SHUTDOWN_POLL) {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                log::debug!("Steam listener shutdown requested, bridge exiting");
                break;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, mpsc as std_mpsc};

    #[test]
    fn idle_listener_shutdown_exits_without_peer_events() {
        let (shutdown_tx, shutdown_rx) = std_mpsc::channel();
        let polls = Arc::new(AtomicUsize::new(0));
        let thread_polls = polls.clone();

        let handle = std::thread::spawn(move || {
            run_listener_shutdown_loop(
                || {
                    thread_polls.fetch_add(1, Ordering::Relaxed);
                    true
                },
                shutdown_rx,
            );
        });

        while polls.load(Ordering::Relaxed) == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(shutdown_tx.send(()).is_ok());
        let _ = shutdown_tx.send(());

        let deadline = std::time::Instant::now() + Duration::from_millis(250);
        while !handle.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(
            handle.is_finished(),
            "idle listener bridge did not exit after shutdown"
        );
        handle.join().unwrap();
    }

    #[test]
    fn listener_shutdown_is_idempotent() {
        let (_connection_tx, connection_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = std_mpsc::channel();
        let listener = SteamListener {
            connections: connection_rx,
            shutdown: shutdown_tx,
        };

        listener.shutdown();
        listener.shutdown();
        drop(listener);

        assert!(shutdown_rx.recv_timeout(Duration::from_millis(250)).is_ok());
    }
}
