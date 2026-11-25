//! Network handle: channel endpoints for communicating with the network actor.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use libp2p::PeerId;
use piying::actor::ActorRef;

use crate::actor::commands::NetworkCommand;
use crate::actor::events::ActorEvent;
use crate::actor::network::NetworkActor;

pub static THESPAN_EVENT_DROPS: AtomicU64 = AtomicU64::new(0);
pub static NETWORK_THREAD_PANICS_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Cloneable handle for sending commands to the network actor.
///
/// Both LAN and Steam transports use the same piying-based `NetworkActor`,
/// so this always sends via a piying `ActorRef` mailbox.
///
/// When the last clone is dropped, the network actor is killed, which
/// stops the background thread and frees the libp2p swarm + tokio runtime.
#[derive(Clone)]
pub struct CommandHandle(ActorRef<NetworkActor>);

impl CommandHandle {
    /// Create a command handle backed by a piying actor.
    pub(crate) fn from_piying(actor_ref: ActorRef<NetworkActor>) -> Self {
        Self(actor_ref)
    }

    /// Send a command to the network actor (non-blocking).
    pub fn send(&self, cmd: NetworkCommand) {
        if let Err(e) = self.0.tell(cmd).try_send() {
            log::warn!("Failed to send NetworkCommand to actor: {e}");
        }
    }

    /// Kill the network actor immediately.
    ///
    /// This aborts the actor's task, causing `prepared.run().await` to return
    /// on the background thread. The tokio runtime and libp2p swarm are then
    /// dropped, and the thread exits.
    pub fn kill(&self) {
        self.0.kill();
    }
}

/// Handle for communicating with a running network actor.
///
/// Returned by [`spawn_network_actor`](crate::spawn_network_actor).
/// Framework integrations (e.g. Bevy) can wrap this in their own resource type.
pub struct NetworkHandle {
    /// Cloneable command sender.
    pub command: CommandHandle,
    /// Receive events from the actor.
    pub event_rx: crossbeam_channel::Receiver<ActorEvent>,
    /// Our local libp2p `PeerId`.
    pub local_peer_id: PeerId,
    /// Local libp2p Ed25519 public key bytes for application-level signatures.
    pub identity_public_key: Option<[u8; 32]>,
    /// Clone of the local libp2p identity keypair for application-level signing.
    pub identity_keypair: Option<libp2p::identity::Keypair>,
    /// Whether this actor was configured with `GossipSub` support.
    pub gossipsub_enabled: bool,
    /// Sender for this handle's piying remote swarm.
    ///
    /// Batch tests can run several network actors in one process. Callers must
    /// use this sender when constructing direct remote refs so messages are
    /// enqueued on the correct actor's swarm instead of the process-global one.
    pub swarm_sender: piying::remote::SwarmSender,
    /// Shared keepalive telemetry published by the libp2p transport thread.
    pub keep_alive_telemetry: crate::actor::behavior::KeepAliveTelemetryHandle,
    /// Background thread running the tokio runtime + libp2p swarm.
    ///
    /// Joining this handle after killing the actor ensures the swarm is fully
    /// torn down and the TCP listener is closed before any new session starts.
    /// Without this, port reuse can cause cross-session connections in tests.
    pub thread_handle: Option<std::thread::JoinHandle<()>>,
}

pub struct NetworkHandleParts {
    pub command: CommandHandle,
    pub event_rx: crossbeam_channel::Receiver<ActorEvent>,
    pub local_peer_id: PeerId,
    pub identity_public_key: Option<[u8; 32]>,
    pub identity_keypair: Option<libp2p::identity::Keypair>,
    pub gossipsub_enabled: bool,
    pub swarm_sender: piying::remote::SwarmSender,
    pub keep_alive_telemetry: crate::actor::behavior::KeepAliveTelemetryHandle,
    pub thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl NetworkHandle {
    /// Send a command to the network actor.
    pub fn send(&self, cmd: NetworkCommand) {
        self.command.send(cmd);
    }

    pub fn event_drops_total() -> u64 {
        THESPAN_EVENT_DROPS.load(Ordering::Relaxed)
    }

    pub fn network_thread_panics_total() -> u64 {
        NETWORK_THREAD_PANICS_TOTAL.load(Ordering::Relaxed)
    }

    pub fn keepalive_mutex_contention_total() -> u64 {
        crate::actor::behavior::KEEPALIVE_MUTEX_CONTENTION_TOTAL.load(Ordering::Relaxed)
    }

    #[expect(
        unsafe_code,
        reason = "moves fields out while deliberately suppressing the Drop implementation"
    )]
    pub fn into_parts(self) -> NetworkHandleParts {
        let this = std::mem::ManuallyDrop::new(self);
        // SAFETY: `self` has been wrapped in `ManuallyDrop`, so `Drop` will not
        // run and none of these fields will be observed or dropped through
        // `this` after the move. Each `ptr::read` moves exactly one owned field
        // into `NetworkHandleParts`, preserving the original ownership.
        let command = unsafe { std::ptr::read(&raw const this.command) };
        // SAFETY: see `command`; this moves the receiver out of the suppressed
        // `NetworkHandle` destructor without aliasing or double-dropping it.
        let event_rx = unsafe { std::ptr::read(&raw const this.event_rx) };
        // SAFETY: see `command`; this moves the optional identity keypair once.
        let identity_keypair = unsafe { std::ptr::read(&raw const this.identity_keypair) };
        // SAFETY: see `command`; this moves the actor-local swarm sender once.
        let swarm_sender = unsafe { std::ptr::read(&raw const this.swarm_sender) };
        // SAFETY: see `command`; this moves the shared telemetry handle once.
        let keep_alive_telemetry = unsafe { std::ptr::read(&raw const this.keep_alive_telemetry) };
        // SAFETY: see `command`; ownership of the join handle is transferred to
        // the caller, so `NetworkHandle::drop` must not also join it.
        let thread_handle = unsafe { std::ptr::read(&raw const this.thread_handle) };
        NetworkHandleParts {
            command,
            event_rx,
            local_peer_id: this.local_peer_id,
            identity_public_key: this.identity_public_key,
            identity_keypair,
            gossipsub_enabled: this.gossipsub_enabled,
            swarm_sender,
            keep_alive_telemetry,
            thread_handle,
        }
    }
}

/// How long teardown waits for the network thread before detaching it.
const NETWORK_THREAD_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Join the network thread, detaching it if it has not exited within
/// [`NETWORK_THREAD_JOIN_TIMEOUT`], so a wedged actor cannot hang test
/// teardown or process exit.
pub fn join_network_thread(handle: std::thread::JoinHandle<()>) {
    join_network_thread_within(handle, NETWORK_THREAD_JOIN_TIMEOUT);
}

/// Returns `false` if the thread was still running at the deadline and was
/// detached.
fn join_network_thread_within(handle: std::thread::JoinHandle<()>, timeout: Duration) -> bool {
    // ponytail: std has no timed join, so poll `is_finished`. A detached thread
    // keeps its swarm and sockets until process exit; if this ever logs, find
    // the await that keeps the killed actor alive.
    let deadline = Instant::now() + timeout;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            tracing::error!(
                target: "thespan::lifecycle",
                ?timeout,
                "network actor thread did not exit after kill; detaching it"
            );
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    match handle.join() {
        Ok(()) => {}
        Err(payload) => {
            NETWORK_THREAD_PANICS_TOTAL.fetch_add(1, Ordering::Relaxed);
            let payload_str = payload
                .downcast_ref::<&'static str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("<non-string panic payload>");
            tracing::error!(
                target: "thespan::lifecycle",
                payload = payload_str,
                "network actor thread panicked"
            );
        }
    }
    true
}

#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub fn test_join_panicking_thread() -> u64 {
    let before = NETWORK_THREAD_PANICS_TOTAL.load(Ordering::Relaxed);
    let handle = std::thread::Builder::new()
        .name("thespan-panic-count-test".into())
        .spawn(|| panic!("injected network actor panic"))
        .expect("spawn panic-count test thread");
    join_network_thread(handle);
    NETWORK_THREAD_PANICS_TOTAL
        .load(Ordering::Relaxed)
        .saturating_sub(before)
}

impl Drop for NetworkHandle {
    fn drop(&mut self) {
        self.command.send(NetworkCommand::Shutdown);
        self.command.kill();
        if let Some(handle) = self.thread_handle.take() {
            join_network_thread(handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::join_network_thread_within;

    #[test]
    fn join_detaches_a_thread_that_never_exits() {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let stuck = std::thread::spawn(move || {
            let _ = wait.recv();
        });
        let started = Instant::now();
        assert!(!join_network_thread_within(
            stuck,
            Duration::from_millis(50)
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(release);

        let done = std::thread::spawn(|| {});
        assert!(join_network_thread_within(done, Duration::from_secs(5)));
    }
}
