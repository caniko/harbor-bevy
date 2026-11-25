//! Bounded typed ingress and the session actor's transport lifetime.

use std::{
    collections::{HashSet, VecDeque},
    marker::PhantomData,
    sync::{
        Arc, LazyLock, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

use bevy::prelude::*;
use harbor_bevy_core::observer_ordering::ObserverOrderingPlugin;
use libp2p::identity::{Keypair, ed25519};
use piying::{
    actor::{ActorId, ActorRef, Spawn as _},
    message::{Context, Message},
    remote::{self, RemoteActor},
};
use thespan::{CommandHandle, PeerId, RemoteTellSendError, TransportError, TransportType};
use thespan_bevy::{FromNetwork, NetworkResource};
use veritasium_wire::PeerIdBytes;

use crate::{
    ConnectionPhase, GameNet, NetConfig, SessionCleanupRequested, SessionFault, SessionRouter,
};

/// Leave space for framing, sender key and signature in the transport limit.
pub const MAX_SESSION_PACKET_BYTES: usize = thespan::DIRECT_SESSION_MAX_BYTES - 256;
/// Well-known wire slot, outside the sequence range in normal use by Piying.
/// Slot zero can alias Piying's first locally generated actor during teardown.
pub const SESSION_ACTOR_SEQUENCE: u64 = u64::MAX;
static SESSION_ACTORS: LazyLock<Mutex<HashSet<ActorId>>> = LazyLock::new(Mutex::default);

/// Outbound control message. The application routes it over its typed
/// session protocol and admits incoming `FromNetwork` messages in its router.
#[derive(Debug, Clone, Event)]
pub struct SendControl<T: Event + Clone> {
    pub to: PeerIdBytes,
    pub message: T,
}

impl<T: Event + Clone> SendControl<T> {
    pub fn new(to: PeerIdBytes, message: T) -> Self {
        Self { to, message }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressLane {
    Reliable,
    Telemetry,
}

pub struct PendingEvent {
    lane: IngressLane,
    ready: Option<Box<dyn Fn(&World) -> bool + Send + Sync>>,
    trigger: Box<dyn FnOnce(&mut World) + Send + Sync>,
}

impl PendingEvent {
    pub fn new<T: Event + Clone>(lane: IngressLane, from: Option<PeerIdBytes>, inner: T) -> Self {
        Self {
            lane,
            ready: None,
            trigger: Box::new(move |world| world.trigger(FromNetwork { inner, from })),
        }
    }

    /// A reliable event blocks later reliable delivery until this predicate
    /// is true. Telemetry that is not ready is discarded.
    pub fn defer_until(mut self, ready: impl Fn(&World) -> bool + Send + Sync + 'static) -> Self {
        self.ready = Some(Box::new(ready));
        self
    }
}

#[derive(Default)]
struct Queues {
    reliable: VecDeque<PendingEvent>,
    telemetry: VecDeque<PendingEvent>,
    deferred: VecDeque<PendingEvent>,
    fault: Option<SessionFault>,
    failed: bool,
    attached: bool,
}

struct Ingress {
    capacity: usize,
    queues: Mutex<Queues>,
    shutting_down: AtomicBool,
    telemetry_dropped: AtomicU64,
}

/// Shared between the actor and Bevy. Enqueueing never waits for queue space.
#[derive(Clone, Resource)]
pub struct SessionIngressHandle(Arc<Ingress>);

impl SessionIngressHandle {
    pub fn new(capacity: usize) -> Self {
        Self(Arc::new(Ingress {
            capacity,
            queues: Mutex::default(),
            shutting_down: AtomicBool::new(false),
            telemetry_dropped: AtomicU64::new(0),
        }))
    }

    fn queues(&self) -> MutexGuard<'_, Queues> {
        self.0.queues.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn is_shutting_down(&self) -> bool {
        self.0.shutting_down.load(Ordering::Acquire)
    }

    /// Capacity drops since this ingress was created, including at capacity zero.
    pub fn telemetry_dropped_count(&self) -> u64 {
        self.0.telemetry_dropped.load(Ordering::Relaxed)
    }

    pub fn inject<T: Event + Clone>(
        &self,
        lane: IngressLane,
        from: Option<PeerIdBytes>,
        event: T,
    ) -> Result<(), SessionFault> {
        self.push(PendingEvent::new(lane, from, event))
    }

    pub fn push(&self, event: PendingEvent) -> Result<(), SessionFault> {
        let mut queues = self.queues();
        if self.is_shutting_down() || queues.failed {
            return Err(SessionFault::ValidationViolation(
                "session ingress is closed".into(),
            ));
        }
        let cap = self.0.capacity;
        let dropped = match event.lane {
            IngressLane::Reliable if queues.reliable.len() >= cap => {
                queues.failed = true;
                queues.fault = Some(SessionFault::OverflowDisconnect);
                return Err(SessionFault::OverflowDisconnect);
            }
            IngressLane::Reliable => {
                queues.reliable.push_back(event);
                None
            }
            IngressLane::Telemetry if cap == 0 => Some(event),
            IngressLane::Telemetry => {
                let dropped = if queues.telemetry.len() == cap {
                    queues.telemetry.pop_front()
                } else {
                    None
                };
                queues.telemetry.push_back(event);
                dropped
            }
        };
        if dropped.is_some() {
            self.0.telemetry_dropped.fetch_add(1, Ordering::Relaxed);
        }
        drop(queues);
        drop(dropped);
        Ok(())
    }

    fn fault(&self, fault: SessionFault) {
        let mut queues = self.queues();
        if !queues.failed && !self.is_shutting_down() {
            queues.failed = true;
            queues.fault = Some(fault);
        }
    }

    fn close(&self) {
        self.0.shutting_down.store(true, Ordering::Release);
        // Release queued events outside the lock.
        let queued = std::mem::take(&mut *self.queues());
        drop(queued);
    }
}

#[derive(piying::Actor)]
pub struct SessionActor<R: SessionRouter> {
    router: R,
    ingress: SessionIngressHandle,
    recipient: PeerId,
    domain: String,
}

impl<R: SessionRouter> RemoteActor for SessionActor<R> {
    const REMOTE_ID: &'static str = "harbor-bevy-net/session/v1";
}

// Piying's link-time message registry needs a concrete monomorphization.
// Erase just the router there; the application and loopback APIs remain typed.
type WireActor = SessionActor<Box<dyn SessionRouter>>;

impl SessionRouter for Box<dyn SessionRouter> {
    fn accepts_peer(&self, peer: &PeerIdBytes) -> bool {
        (**self).accepts_peer(peer)
    }

    fn route(
        &mut self,
        from: PeerIdBytes,
        packet: &[u8],
    ) -> Result<Option<PendingEvent>, SessionFault> {
        (**self).route(from, packet)
    }
}

#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct SessionFrame {
    public_key: [u8; 32],
    signature: [u8; 64],
    packet: Vec<u8>,
}

fn frame_digest(domain: &str, recipient: PeerId, packet: &[u8]) -> blake3::Hash {
    let mut hash = blake3::Hasher::new_derive_key("harbor-bevy-net signed session frame v1");
    let recipient = recipient.to_bytes();
    for part in [domain.as_bytes(), &recipient, packet] {
        hash.update(&(part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    hash.finalize()
}

impl SessionFrame {
    fn sign(
        signer: &Keypair,
        domain: &str,
        recipient: PeerId,
        packet: Vec<u8>,
    ) -> Result<Self, SessionSendError> {
        if packet.len() > MAX_SESSION_PACKET_BYTES {
            return Err(SessionSendError::Oversized);
        }
        let key = signer
            .clone()
            .try_into_ed25519()
            .map_err(|_| SessionSendError::Identity)?;
        let digest = frame_digest(domain, recipient, &packet);
        Ok(Self {
            public_key: key.public().to_bytes(),
            signature: key
                .sign(digest.as_bytes())
                .try_into()
                .map_err(|_| SessionSendError::Identity)?,
            packet,
        })
    }

    fn sender(&self, domain: &str, recipient: PeerId) -> Option<PeerIdBytes> {
        if self.packet.len() > MAX_SESSION_PACKET_BYTES {
            return None;
        }
        let key = ed25519::PublicKey::try_from_bytes(&self.public_key).ok()?;
        let digest = frame_digest(domain, recipient, &self.packet);
        if !key.verify(digest.as_bytes(), &self.signature) {
            return None;
        }
        let peer = libp2p::identity::PublicKey::from(key).to_peer_id();
        Some(peer.to_bytes().into())
    }
}

#[piying::remote_message("session-frame-v1")]
impl Message<SessionFrame> for WireActor {
    type Reply = ();

    async fn handle(&mut self, frame: SessionFrame, _: &mut Context<Self, Self::Reply>) {
        if self.ingress.is_shutting_down() {
            return;
        }
        // Piying 0.1 does not expose the transport source in Message::Context.
        let Some(from) = frame.sender(&self.domain, self.recipient) else {
            return;
        };
        if !self.router.accepts_peer(&from) {
            return;
        }
        match self.router.route(from, &frame.packet) {
            Ok(Some(event)) => {
                let _ = self.ingress.push(event);
            }
            Ok(None) => {}
            Err(fault) => self.ingress.fault(fault),
        }
    }
}

#[derive(Resource)]
pub struct RemoteSessionRef<R: SessionRouter> {
    peer_id: PeerId,
    sender: CommandHandle,
    router: PhantomData<fn() -> R>,
}

impl<R: SessionRouter> RemoteSessionRef<R> {
    pub fn for_peer_with_sender(peer_id: PeerId, sender: CommandHandle) -> Self {
        Self {
            peer_id,
            sender,
            router: PhantomData,
        }
    }

    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    /// Reserve a slot in the transport's reliable outbound window without
    /// waiting. Later remote rejections surface as [`crate::RemoteSendFailedEvent`];
    /// application-level delivery still requires a protocol acknowledgement.
    pub fn send(
        &self,
        config: &NetConfig,
        signer: &Keypair,
        packet: Vec<u8>,
    ) -> Result<(), SessionSendError> {
        let frame = SessionFrame::sign(
            signer,
            &config.control_domain("direct-v1"),
            self.peer_id,
            packet,
        )?;
        self.sender
            .send_remote_tell::<WireActor, _>(self.peer_id, SESSION_ACTOR_SEQUENCE, &frame)
            .map_err(|error| match error {
                RemoteTellSendError::Full => SessionSendError::Full,
                RemoteTellSendError::Closed => SessionSendError::Closed,
                RemoteTellSendError::Encode(error) => SessionSendError::Remote(error),
            })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionSendError {
    #[error("Reliable outbound window or command mailbox is full")]
    Full,
    #[error("Session transport is closed")]
    Closed,
    #[error("Session frame exceeds the transport limit")]
    Oversized,
    #[error("Session needs an Ed25519 identity")]
    Identity,
    #[error("Could not encode the session frame: {0}")]
    Remote(piying::error::RemoteSendError),
}

/// Observes transport-owned session teardown without stopping the transport.
#[derive(Clone)]
pub struct SessionTeardownHandle(Arc<AtomicBool>);

impl SessionTeardownHandle {
    /// True after the actor stopped and its well-known ID can be reused.
    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

struct SessionRegistration {
    id: ActorId,
    actor: ActorRef<WireActor>,
    task: tokio::task::JoinHandle<
        Result<(WireActor, piying::error::ActorStopReason), piying::error::PanicError>,
    >,
    ingress: SessionIngressHandle,
    teardown: SessionTeardownHandle,
}

impl SessionRegistration {
    async fn shutdown(self) {
        self.ingress.close();
        self.actor.kill();
        let mut backoff = Duration::from_millis(1);
        while let UnregisterResult::Contended = unregister(self.id).await {
            // Cap the delay, not the attempts: abandoning a live entry would
            // leak the strong actor reference in Piying's registry.
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_millis(8));
        }
        // The task returns the actor itself; drop it while Tokio is still live.
        drop(self.task.await);
        SESSION_ACTORS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
        self.teardown.0.store(true, Ordering::Release);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum UnregisterResult {
    AlreadyUnregistered,
    Removed,
    Contended,
}

async fn unregister(id: ActorId) -> UnregisterResult {
    if remote::unregister_actor_local(&id) {
        return UnregisterResult::Removed;
    }
    // Piying 0.1.3's public try-remove and presence check both return false
    // on contention. Its dispatch helper takes the short registry lock and
    // looks up the strong ref before decoding. An empty archive cannot decode,
    // so this probe never sends to the mailbox.
    if matches!(
        remote::_internal::try_tell::<WireActor, SessionFrame>(id, Vec::new()).await,
        Err(piying::error::RemoteSendError::ActorNotRunning)
    ) {
        UnregisterResult::AlreadyUnregistered
    } else {
        UnregisterResult::Contended
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionStartError {
    #[error("Session actor mailbox capacity must be nonzero")]
    MailboxCapacity,
    #[error("Session actor is already attached")]
    AlreadyAttached,
    #[error("Session actor needs an Ed25519 identity")]
    Identity,
    #[error("Session actor did not initialize")]
    NotStarted,
    #[error(transparent)]
    LoadIdentity(#[from] thespan::identity::IdentityError),
    #[error(transparent)]
    Transport(#[from] TransportError),
}

/// Start the actor in the transport's pre-run hook and attach its teardown
/// before returning the resource. Insert the ingress in the app before polling.
pub fn spawn_session<R: SessionRouter>(
    config: &NetConfig,
    transport: TransportType,
    keypair: Option<Keypair>,
    router: R,
    ingress: SessionIngressHandle,
) -> Result<(NetworkResource, SessionTeardownHandle), SessionStartError> {
    if config.mailbox_capacity == 0 {
        return Err(SessionStartError::MailboxCapacity);
    }
    let transport_config = config.to_thespan_config();
    let keypair = match keypair {
        Some(keypair) => keypair,
        None => transport_config.load_or_create_keypair()?,
    };
    if keypair.public().try_into_ed25519().is_err() {
        return Err(SessionStartError::Identity);
    }
    {
        let mut queues = ingress.queues();
        if queues.attached || ingress.is_shutting_down() {
            return Err(SessionStartError::AlreadyAttached);
        }
        queues.attached = true;
    }
    let domain = config.control_domain("direct-v1");
    let capacity = config.mailbox_capacity;
    let hook_ingress = ingress.clone();
    let (tx, rx) = mpsc::sync_channel(1);
    let transport = thespan_bevy::spawn_with_lifecycle(
        transport,
        transport_config,
        Some(keypair),
        Some(Box::new(move |peer_id| {
            match register(peer_id, Box::new(router), domain, capacity, hook_ingress) {
                Ok(registration) => {
                    let _ = tx.send(Ok(registration.teardown.clone()));
                    Some(Box::pin(registration.shutdown()))
                }
                Err(error) => {
                    let _ = tx.send(Err(error));
                    None
                }
            }
        })),
    );
    let started = rx.try_recv().unwrap_or(Err(SessionStartError::NotStarted));
    match (transport, started) {
        (Ok(network), Ok(teardown)) => Ok((network, teardown)),
        (Ok(mut network), Err(error)) => {
            ingress.close();
            network.shutdown_now();
            Err(error)
        }
        (Err(error), started) => {
            if started.is_err() {
                ingress.close();
            }
            Err(error.into())
        }
    }
}

fn register(
    recipient: PeerId,
    router: Box<dyn SessionRouter>,
    domain: String,
    capacity: usize,
    ingress: SessionIngressHandle,
) -> Result<SessionRegistration, SessionStartError> {
    let id = ActorId::new_with_peer_id(SESSION_ACTOR_SEQUENCE, recipient);
    if !SESSION_ACTORS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(id)
    {
        return Err(SessionStartError::AlreadyAttached);
    }
    let prepared = SessionActor::prepare_with_mailbox(piying::mailbox::bounded(capacity));
    let actor = prepared.actor_ref().clone();
    remote::register_actor_local_sync(&actor, id);
    let task = prepared.spawn(SessionActor {
        router,
        ingress: ingress.clone(),
        recipient,
        domain,
    });
    Ok(SessionRegistration {
        id,
        actor,
        task,
        ingress,
        teardown: SessionTeardownHandle(Arc::new(AtomicBool::new(false))),
    })
}

pub struct SessionPlugin<G: GameNet>(PhantomData<fn() -> G>);

impl<G: GameNet> Default for SessionPlugin<G> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<G: GameNet> Plugin for SessionPlugin<G> {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<ObserverOrderingPlugin>() {
            app.add_plugins(ObserverOrderingPlugin);
        }
        app.init_resource::<NetConfig>();
        let capacity = app.world().resource::<NetConfig>().ingress_capacity;
        if !app.world().contains_resource::<SessionIngressHandle>() {
            app.insert_resource(SessionIngressHandle::new(capacity));
        }
        app.add_observer(reset_ingress).add_systems(PreUpdate, pump);
    }
}

fn reset_ingress(
    _: On<SessionCleanupRequested>,
    mut ingress: ResMut<SessionIngressHandle>,
    config: Res<NetConfig>,
) {
    ingress.close();
    *ingress = SessionIngressHandle::new(config.ingress_capacity);
}

fn pump(world: &mut World) {
    let config = world.resource::<NetConfig>();
    let (budget, deferred_cap) = (config.max_events_per_frame, config.max_deferred_events);
    let ingress = world.resource::<SessionIngressHandle>().clone();
    let mut blocked = false;
    // Check the fault even if the frame budget is zero or the last event
    // discovers a deferred overflow.
    for index in 0..=budget {
        let (fault, pending) = {
            let mut queues = ingress.queues();
            let pending = if index == budget || queues.failed || ingress.is_shutting_down() {
                None
            } else {
                (!blocked)
                    .then(|| queues.deferred.pop_front())
                    .flatten()
                    .map(|event| (event, true))
                    .or_else(|| queues.reliable.pop_front().map(|event| (event, false)))
                    .or_else(|| queues.telemetry.pop_front().map(|event| (event, false)))
            };
            (queues.fault.take(), pending)
        };
        if let Some(fault) = fault {
            world.trigger(fault);
            world.flush();
            return;
        }
        let Some((event, was_deferred)) = pending else {
            return;
        };
        let reliable = event.lane == IngressLane::Reliable;
        if !(blocked && reliable) && event.ready.as_ref().is_none_or(|ready| ready(world)) {
            (event.trigger)(world);
            // The fork defers Commands from observers. A packet may install
            // a snapshot barrier or disconnect before the next packet runs.
            world.flush();
            if matches!(
                world.get_resource::<NextState<ConnectionPhase>>(),
                Some(NextState::Pending(ConnectionPhase::Disconnecting))
            ) {
                return;
            }
        } else if reliable {
            blocked = true;
            let overflow = {
                let mut queues = ingress.queues();
                if ingress.is_shutting_down() {
                    return;
                }
                if was_deferred {
                    queues.deferred.push_front(event);
                    false
                } else if queues.deferred.len() < deferred_cap {
                    queues.deferred.push_back(event);
                    false
                } else {
                    true
                }
            };
            if overflow {
                ingress.fault(SessionFault::OverflowDisconnect);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unregister_distinguishes_an_absent_actor_from_contention() {
        let id = ActorId::new_with_peer_id(SESSION_ACTOR_SEQUENCE, PeerId::random());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert_eq!(
            runtime.block_on(unregister(id)),
            UnregisterResult::AlreadyUnregistered
        );
    }

    #[test]
    fn signature_binds_sender_recipient_scope_and_payload() {
        let key = Keypair::generate_ed25519();
        let recipient = Keypair::generate_ed25519().public().to_peer_id();
        let other = Keypair::generate_ed25519().public().to_peer_id();
        let mut frame = SessionFrame::sign(&key, "scope", recipient, vec![1]).unwrap();
        let sender = PeerIdBytes::from(key.public().to_peer_id().to_bytes());
        assert_eq!(frame.sender("scope", recipient), Some(sender));
        assert_eq!(frame.sender("scope", other), None);
        assert_eq!(frame.sender("other scope", recipient), None);
        frame.packet[0] = 2;
        assert_eq!(frame.sender("scope", recipient), None);
    }
}
