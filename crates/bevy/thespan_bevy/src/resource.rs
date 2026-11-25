//! Bevy Resource for communicating with the background network actor.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use bevy::prelude::*;
use thespan::{
    CommandHandle, CommandSendError, EventReceiver, GossipTopic, Multiaddr, NetworkCommand, PeerId,
};

/// How often [`NetworkResource::shutdown_now`] wakes to look for the transport
/// thread's exit while draining events.
const DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Teardown drain callbacks for bounded channels owned by higher layers.
///
/// [`NetworkResource::shutdown_now`] only knows the transport event channel.
/// Session dispatch writes into a different bounded channel from the same
/// network thread, so draining only `event_rx` leaves those senders stalled on
/// a full channel for the whole shutdown. The owner of each such channel
/// registers a drain callback here instead.
#[derive(Default)]
pub struct ShutdownDrains(Vec<Box<dyn Fn() + Send + Sync>>);

impl ShutdownDrains {
    /// Register a callback that consumes one layer's pending teardown events.
    pub fn add(&mut self, drain: impl Fn() + Send + Sync + 'static) {
        self.0.push(Box::new(drain));
    }

    /// Run every registered drain. Events are discarded: the consumer is gone.
    pub fn run(&self) {
        for drain in &self.0 {
            drain();
        }
    }
}

/// Connection state of the network layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransportConnectionStatus {
    #[default]
    Disconnected,
    Connected,
}

/// Bevy resource holding channels to communicate with the background network actor.
///
/// Insert the resource returned by `spawn_network_actor` into the application.
/// Removed on disconnect/shutdown.
#[derive(Resource)]
pub struct NetworkResource {
    /// Send commands to the actor (consumer -> Actor).
    pub command: CommandHandle,
    /// Receive events from the actor (Actor -> consumer).
    pub event_rx: EventReceiver,
    /// Current connection state.
    pub state: TransportConnectionStatus,
    /// All currently connected peers.
    pub peers: HashSet<PeerId>,
    /// Remote address of each connected peer (for building mesh dial addresses).
    pub peer_addrs: HashMap<PeerId, Multiaddr>,
    /// Our local libp2p `PeerId`.
    pub local_peer_id: PeerId,
    /// Local libp2p Ed25519 public key bytes for application-level signatures.
    pub identity_public_key: Option<[u8; 32]>,
    /// Clone of the local libp2p identity keypair for application-level signing.
    pub identity_keypair: Option<thespan::identity::Keypair>,
    /// Whether the underlying transport has `GossipSub` enabled.
    pub gossipsub_enabled: bool,
    /// Raw Piying sender (not backpressured). Use `command` for reliable tells.
    pub swarm_sender: thespan::SwarmSender,
    /// Shared keepalive telemetry handle owned by the transport thread.
    pub keep_alive_telemetry: thespan::KeepAliveTelemetryHandle,
    /// Address we're listening on (set when `ListenStartedEvent` fires).
    pub listen_addr: Option<thespan::Multiaddr>,
    /// Background thread handle for synchronous shutdown.
    ///
    /// Joined on drop to ensure the libp2p swarm and TCP listener are fully
    /// torn down before a new session can reuse the same port.
    pub(crate) thread_handle: Option<std::thread::JoinHandle<()>>,
    /// Teardown drains for bounded channels owned by higher layers (session
    /// dispatch), drained while the transport thread exits.
    pub(crate) shutdown_drains: ShutdownDrains,
}

#[derive(Debug, Clone, Copy)]
pub struct KeepAlivePeerTelemetry {
    pub pings_sent: u64,
    pub pings_received: u64,
    pub last_ping_at: Option<std::time::Instant>,
    pub idle_window_observations: u64,
}

/// Per-peer keepalive telemetry mirrored into Bevy from the libp2p transport.
#[derive(Resource, Debug, Default, Clone)]
pub struct KeepAliveTelemetry {
    pub peers: HashMap<PeerId, KeepAlivePeerTelemetry>,
}

#[derive(Resource, Debug, Default, Clone)]
pub struct OversizedMessageDrops {
    pub peers: HashMap<PeerId, u64>,
}

impl OversizedMessageDrops {
    pub(crate) fn record(&mut self, peer_id: PeerId, total_for_peer: u64) {
        self.peers.insert(peer_id, total_for_peer);
    }
}

impl KeepAliveTelemetry {
    pub fn pings_sent(&self) -> u64 {
        self.peers.values().map(|peer| peer.pings_sent).sum()
    }

    pub fn pings_received(&self) -> u64 {
        self.peers.values().map(|peer| peer.pings_received).sum()
    }

    pub fn idle_window_observations(&self) -> u64 {
        self.peers
            .values()
            .map(|peer| peer.idle_window_observations)
            .sum()
    }

    pub fn export_counts(&self) -> BTreeMap<String, (u64, u64, u64)> {
        self.peers
            .iter()
            .map(|(peer_id, peer)| {
                (
                    peer_id.to_string(),
                    (
                        peer.pings_sent,
                        peer.pings_received,
                        peer.idle_window_observations,
                    ),
                )
            })
            .collect()
    }

    pub(crate) fn sync_from_transport(&mut self, snapshots: Vec<thespan::KeepAlivePeerTelemetry>) {
        self.peers = snapshots
            .into_iter()
            .map(|peer| {
                (
                    peer.peer_id,
                    KeepAlivePeerTelemetry {
                        pings_sent: peer.pings_sent,
                        pings_received: peer.pings_received,
                        last_ping_at: peer.last_ping_at,
                        idle_window_observations: peer.idle_window_observations,
                    },
                )
            })
            .collect();
    }
}

impl NetworkResource {
    /// Send without waiting. On overflow the command was not accepted.
    pub fn send(&self, cmd: NetworkCommand) -> Result<(), CommandSendError> {
        self.command.send(cmd)
    }

    /// Check if any peers are connected.
    pub fn has_peers(&self) -> bool {
        !self.peers.is_empty()
    }

    /// Check if a specific peer is connected.
    pub fn is_connected(&self, peer_id: &PeerId) -> bool {
        self.peers.contains(peer_id)
    }

    /// Register a teardown drain for a bounded channel fed by the network
    /// thread (e.g. the session event channel).
    ///
    /// [`shutdown_now`](Self::shutdown_now) runs the callback before stopping
    /// the transport actor, then again while its thread exits, so blocking
    /// producers never stall on a full channel.
    pub fn register_shutdown_drain(&mut self, drain: impl Fn() + Send + Sync + 'static) {
        self.shutdown_drains.add(drain);
    }

    /// Stop the background network actor and wait for its thread to exit.
    ///
    /// Normally this runs from `Drop`, but shutdown observers call it explicitly
    /// before process exit so transport resources are released synchronously.
    pub fn shutdown_now(&mut self) {
        let Some(handle) = self.thread_handle.take() else {
            return;
        };
        // These callbacks only drain channels; runtime-owned registrations use
        // the transport lifecycle hook, including on independent actor exit.
        self.shutdown_drains.run();
        // Kill the piying NetworkActor. This aborts its task, which makes
        // `prepared.run().await` return on the background thread. Lifecycle
        // cleanup runs there before the Tokio runtime drops.
        self.command.kill();

        // Keep consuming events while the background thread winds down.
        //
        // Session actors run on that thread and push into bounded channels from
        // inside their message handlers. With no consumer, a full channel makes
        // every queued message wait out the send timeout before the actor can
        // observe the kill, so a backlogged mailbox keeps the thread alive for
        // minutes and `join_network_thread` (test teardown, process exit)
        // appears hung. These events are discarded: the Bevy consumer is
        // already gone.
        // Bounded like `join_network_thread`: a wedged thread must not turn
        // the drain into the infinite wait the join deadline exists to stop.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        drain_events_until(
            || {
                let _ = self.event_rx.recv_timeout(DRAIN_POLL_INTERVAL);
            },
            &self.shutdown_drains,
            || handle.is_finished() || std::time::Instant::now() >= deadline,
        );

        // Wait for the background thread to fully exit. This guarantees the
        // libp2p swarm and transport sockets are torn down before process exit.
        thespan::handle::join_network_thread(handle);
    }
}

impl Drop for NetworkResource {
    fn drop(&mut self) {
        self.shutdown_now();
    }
}

/// Drain `event_rx` (and every registered teardown drain) until `is_finished`
/// reports the transport thread exited.
///
/// Teardown-only: every event is discarded, but consuming them keeps the
/// actors' bounded `send_timeout` pushes from stalling on a full channel.
fn drain_events_until(
    mut poll: impl FnMut(),
    shutdown_drains: &ShutdownDrains,
    is_finished: impl Fn() -> bool,
) {
    while !is_finished() {
        shutdown_drains.run();
        poll();
    }
}

/// Bevy event triggered when a peer connects.
#[derive(Event, Debug, Clone)]
pub struct PeerConnectedEvent {
    pub peer_id: PeerId,
    /// Remote address of the connected peer.
    pub addr: Multiaddr,
}

/// Bevy event triggered when a peer disconnects.
#[derive(Event, Debug, Clone)]
pub struct PeerDisconnectedEvent {
    pub peer_id: PeerId,
    pub reason: String,
}

/// Bevy event triggered when the swarm starts listening.
#[derive(Event, Debug, Clone)]
pub struct ListenStartedEvent {
    pub addr: thespan::Multiaddr,
}

/// A remote actor rejected a tell, or its outbound request failed.
#[derive(Event, Debug, Clone)]
pub struct RemoteSendFailedEvent {
    pub peer_id: PeerId,
    pub error: String,
}

/// Bevy event triggered when a `GossipSub` message is received.
#[derive(Event, Debug, Clone)]
pub struct RawGossipMessageEvent {
    /// The application topic name, without the session suffix.
    pub topic: GossipTopic,
    /// The peer who originally published the message (if signed).
    pub source: Option<PeerId>,
    /// The raw payload bytes.
    pub payload: Vec<u8>,
}

/// Bevy event triggered when the local node subscribes to a `GossipSub` topic.
#[derive(Event, Debug, Clone)]
pub struct GossipSubscribedEvent {
    pub topic: GossipTopic,
}

/// Bevy event triggered when local `GossipSub` topic subscription fails.
#[derive(Event, Debug, Clone)]
pub struct GossipSubscribeFailedEvent {
    pub topic: GossipTopic,
    pub reason: String,
}

/// Raw request-response message received from the network actor.
#[cfg(feature = "legacy-raw-codec")]
#[derive(Event, Debug, Clone)]
pub struct RawMessageEvent {
    pub from: PeerId,
    pub payload: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use thespan::ActorEvent;

    /// Regression: teardown used to join the transport thread while its session
    /// actors were blocked pushing into a full event channel, so `Drop` hung
    /// until every queued event burned its send timeout. `shutdown_now` must
    /// keep draining events while the thread exits.
    #[test]
    fn drain_events_until_delivers_backlog_while_the_thread_exits() {
        let capacity = 2;
        let total = 8;
        let (tx, rx) = crossbeam_channel::bounded(capacity);
        let delivered = std::sync::Arc::new(AtomicUsize::new(0));
        let delivered_sender = delivered.clone();

        // The session dispatch channel is a second bounded queue fed by the
        // same thread; teardown must drain it too.
        let (session_tx, session_rx) = crossbeam_channel::bounded::<u8>(1);
        session_tx.send(1).expect("seed the full session channel");
        let mut drains = ShutdownDrains::default();
        let drain_rx = session_rx.clone();
        drains.add(move || while drain_rx.try_recv().is_ok() {});

        let sender = std::thread::spawn(move || {
            for _ in 0..total {
                let event = ActorEvent::ListenStarted {
                    addr: "/memory/0".parse().expect("valid memory multiaddr"),
                };
                // Mirrors the session actor's bounded push during teardown.
                if tx.send_timeout(event, Duration::from_millis(20)).is_ok() {
                    delivered_sender.fetch_add(1, Ordering::Relaxed);
                }
            }
        });

        drain_events_until(
            || {
                let _ = rx.recv_timeout(DRAIN_POLL_INTERVAL);
            },
            &drains,
            || sender.is_finished(),
        );
        sender.join().expect("sender thread joins");

        assert_eq!(
            delivered.load(Ordering::Relaxed),
            total,
            "a draining shutdown must not drop events behind a full channel"
        );
        assert_eq!(
            session_rx.try_recv(),
            Err(crossbeam_channel::TryRecvError::Empty),
            "the shutdown drain must also consume the session dispatch channel"
        );
    }

    /// Regression: session dispatch writes into its own bounded channel, not
    /// the transport event channel. `shutdown_now` must run the registered
    /// drains, otherwise a blocked session producer outlives the join.
    #[test]
    fn registered_shutdown_drains_replace_the_stalled_session_channel() {
        let (tx, rx) = crossbeam_channel::bounded::<u8>(1);
        rx.try_recv().ok();
        tx.send(1).expect("seed the full channel");

        let mut drains = ShutdownDrains::default();
        let drain_rx = rx.clone();
        drains.add(move || while drain_rx.try_recv().is_ok() {});

        drains.run();

        assert_eq!(rx.try_recv(), Err(crossbeam_channel::TryRecvError::Empty));
        assert!(
            tx.send(2).is_ok(),
            "session dispatch must make progress once the teardown drain ran"
        );
    }
}
