//! Discovery client for the Thespan libp2p rendezvous service.
//!
//! ```text
//! ┌─ Client (this crate) ───────────────────────────────────────┐
//! │  background tokio task drives the swarm                     │
//! │  ↓                                                          │
//! │  Command channel ⇄ public async API (announce/discover/…)   │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! The client is dial-only against rendezvous servers: it does not run a
//! relay hop, does not announce on Kademlia, and does not accept inbound
//! rendezvous requests. It does run `relay::client` + `dcutr` so that
//! application dials to *other peers* can use
//! the same swarm for hole punching.

pub mod codec;
mod sign;
mod swarm;

use futures::StreamExt;
use libp2p::{Multiaddr, PeerId, Swarm, identity::Keypair, request_response, swarm::SwarmEvent};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use swarm::{ClientBehaviour, ClientBehaviourEvent};
use thespan_rendezvous_proto::{
    DiscoverRequest, ErrorCode, Frame, HeartbeatRequest, LobbyEntry, Namespace, PeerTicket,
    RegisterRequest, UnregisterRequest,
};
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::{debug, warn};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("client task has shut down")]
    Shutdown,
    #[error("server returned error: {0:?} — {1}")]
    Server(ErrorCode, String),
    #[error("protocol violation: {0}")]
    Protocol(&'static str),
    #[error("sign: {0}")]
    Sign(#[from] sign::SignError),
    #[error("swarm: {0}")]
    Swarm(String),
    #[error("timeout awaiting response")]
    Timeout,
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub keypair: Keypair,
    /// Multiaddrs of known rendezvous servers to dial. At least one required.
    pub bootstraps: Vec<Multiaddr>,
    /// Per-request response timeout.
    pub request_timeout: Duration,
}

impl ClientConfig {
    pub fn new(keypair: Keypair, bootstraps: Vec<Multiaddr>) -> Self {
        Self {
            keypair,
            bootstraps,
            request_timeout: Duration::from_secs(10),
        }
    }
}

pub struct Client {
    cmd_tx: mpsc::Sender<Command>,
    keypair: Keypair,
}

#[allow(clippy::large_enum_variant)]
enum Command {
    Send {
        peer: PeerId,
        frame: Frame,
        respond: oneshot::Sender<Result<Frame, ClientError>>,
    },
    /// First connected bootstrap peer. Resolves once the swarm has at least
    /// one connection to a rendezvous server.
    AnyPeer { respond: oneshot::Sender<PeerId> },
}

impl Client {
    pub async fn start(config: ClientConfig) -> Result<Self, ClientError> {
        if config.bootstraps.is_empty() {
            return Err(ClientError::Protocol(
                "at least one bootstrap address required",
            ));
        }

        let keypair = config.keypair.clone();
        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair.clone())
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default().nodelay(true),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .map_err(|e| ClientError::Swarm(format!("tcp: {e}")))?
            .with_quic()
            .with_relay_client(libp2p::noise::Config::new, libp2p::yamux::Config::default)
            .map_err(|e| ClientError::Swarm(format!("relay client: {e}")))?
            .with_behaviour(ClientBehaviour::new)
            .map_err(|e| ClientError::Swarm(format!("behaviour: {e}")))?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
            .build();

        for (index, addr) in config.bootstraps.iter().enumerate() {
            if index > 0 {
                let delay = bootstrap_dial_delay(index);
                debug!(%addr, ?delay, "staggering bootstrap dial");
                tokio::time::sleep(delay).await;
            }
            if let Err(e) = swarm.dial(addr.clone()) {
                warn!(%addr, ?e, "initial dial failed");
            }
        }

        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>(64);
        tokio::spawn(run_swarm(swarm, cmd_rx, config.request_timeout));

        Ok(Self { cmd_tx, keypair })
    }

    async fn send(&self, peer: PeerId, frame: Frame) -> Result<Frame, ClientError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Send {
                peer,
                frame,
                respond: tx,
            })
            .await
            .map_err(|_| ClientError::Shutdown)?;
        rx.await.map_err(|_| ClientError::Shutdown)?
    }

    async fn any_peer(&self) -> Result<PeerId, ClientError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::AnyPeer { respond: tx })
            .await
            .map_err(|_| ClientError::Shutdown)?;
        rx.await.map_err(|_| ClientError::Shutdown)
    }

    /// Announce the local peer in `namespace`. The returned [`Announcement`]
    /// owns a heartbeat task; drop it to unregister.
    pub async fn announce(
        &self,
        namespace: Namespace,
        ticket: PeerTicket,
        metadata: Vec<u8>,
        ttl: Duration,
    ) -> Result<Announcement, ClientError> {
        let peer = self.any_peer().await?;
        let req = RegisterRequest {
            namespace: namespace.clone(),
            ticket,
            ttl_secs: ttl.as_secs() as u32,
            metadata,
        };
        let env = sign::seal(&self.keypair, &req)?;
        let resp = self.send(peer, Frame::Register(env)).await?;
        match resp {
            Frame::RegisterAck(_) => Ok(Announcement::spawn(
                self.cmd_tx.clone(),
                self.keypair.clone(),
                peer,
                namespace,
                ttl,
            )),
            Frame::Error(e) => Err(ClientError::Server(e.code, e.detail)),
            _ => Err(ClientError::Protocol("expected RegisterAck")),
        }
    }

    pub async fn discover(&self, namespace: Namespace) -> Result<Vec<LobbyEntry>, ClientError> {
        let peer = self.any_peer().await?;
        let mut all = Vec::new();
        let mut cursor = Vec::new();
        loop {
            let req = DiscoverRequest {
                namespace: namespace.clone(),
                cursor: cursor.clone(),
                limit: 64,
            };
            let resp = self.send(peer, Frame::Discover(req)).await?;
            match resp {
                Frame::DiscoverResponse(r) => {
                    all.extend(r.entries);
                    if r.next_cursor.is_empty() {
                        break;
                    }
                    cursor = r.next_cursor;
                }
                Frame::Error(e) => return Err(ClientError::Server(e.code, e.detail)),
                _ => return Err(ClientError::Protocol("expected DiscoverResponse")),
            }
        }
        Ok(all)
    }
}

fn bootstrap_dial_delay(index: usize) -> Duration {
    const BASE_MILLIS: u64 = 100;
    let base = BASE_MILLIS.saturating_mul(index as u64);
    let percent = 75 + ((index as u64 * 37) % 51);
    Duration::from_millis((base.saturating_mul(percent) / 100).max(1))
}

/// Lifetime handle to an active announcement. Drop to unregister cleanly;
/// even without the explicit unregister, the server expires it via TTL.
pub struct Announcement {
    _heartbeat: tokio::task::JoinHandle<()>,
    _drop_tx: oneshot::Sender<()>,
}

impl Announcement {
    fn spawn(
        cmd_tx: mpsc::Sender<Command>,
        keypair: Keypair,
        peer: PeerId,
        namespace: Namespace,
        ttl: Duration,
    ) -> Self {
        let (drop_tx, mut drop_rx) = oneshot::channel::<()>();
        // Heartbeat at half-TTL to comfortably stay before expiry.
        let interval = ttl / 2;
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval.max(Duration::from_secs(5)));
            tick.tick().await; // skip immediate
            loop {
                tokio::select! {
                    _ = &mut drop_rx => {
                        let req = UnregisterRequest { namespace: namespace.clone() };
                        if let Ok(env) = sign::seal(&keypair, &req) {
                            let (tx, _rx) = oneshot::channel();
                            let _ = cmd_tx.send(Command::Send { peer, frame: Frame::Unregister(env), respond: tx }).await;
                        }
                        return;
                    }
                    _ = tick.tick() => {
                        let req = HeartbeatRequest { namespace: namespace.clone(), ttl_secs: ttl.as_secs() as u32 };
                        let Ok(env) = sign::seal(&keypair, &req) else { continue };
                        let (tx, rx) = oneshot::channel();
                        if cmd_tx.send(Command::Send { peer, frame: Frame::Heartbeat(env), respond: tx }).await.is_err() {
                            return;
                        }
                        match rx.await {
                            Ok(Ok(Frame::RegisterAck(_))) => debug!("heartbeat ok"),
                            Ok(Ok(Frame::Error(e))) => warn!(?e, "heartbeat refused"),
                            Ok(Err(e)) => warn!(?e, "heartbeat send err"),
                            _ => {}
                        }
                    }
                }
            }
        });
        Self {
            _heartbeat: task,
            _drop_tx: drop_tx,
        }
    }
}

async fn run_swarm(
    mut swarm: Swarm<ClientBehaviour>,
    mut cmd_rx: mpsc::Receiver<Command>,
    request_timeout: Duration,
) {
    /// Maps an outbound `request_response` `request_id` → caller.
    type Pending =
        HashMap<request_response::OutboundRequestId, oneshot::Sender<Result<Frame, ClientError>>>;
    let pending: Arc<Mutex<Pending>> = Arc::new(Mutex::new(HashMap::new()));
    let mut connected_peers: Vec<PeerId> = Vec::new();
    let mut waiters_for_peer: Vec<oneshot::Sender<PeerId>> = Vec::new();

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { return };
                match cmd {
                    Command::Send { peer, frame, respond } => {
                        let id = swarm.behaviour_mut().rendezvous.send_request(&peer, frame);
                        let pending_clone = Arc::clone(&pending);
                        // Insert + spawn timeout sentinel.
                        pending.lock().await.insert(id, respond);
                        tokio::spawn(async move {
                            tokio::time::sleep(request_timeout).await;
                            if let Some(tx) = pending_clone.lock().await.remove(&id) {
                                let _ = tx.send(Err(ClientError::Timeout));
                            }
                        });
                    }
                    Command::AnyPeer { respond } => {
                        if let Some(p) = connected_peers.first() {
                            let _ = respond.send(*p);
                        } else {
                            waiters_for_peer.push(respond);
                        }
                    }
                }
            }
            event = swarm.select_next_some() => {
                match event {
                    SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                        if !connected_peers.contains(&peer_id) {
                            connected_peers.push(peer_id);
                        }
                        for w in waiters_for_peer.drain(..) {
                            let _ = w.send(peer_id);
                        }
                    }
                    SwarmEvent::ConnectionClosed { peer_id, .. } => {
                        connected_peers.retain(|p| p != &peer_id);
                    }
                    SwarmEvent::Behaviour(ClientBehaviourEvent::Rendezvous(
                        request_response::Event::Message { message, .. },
                    )) => {
                        if let request_response::Message::Response { request_id, response } = message
                            && let Some(tx) = pending.lock().await.remove(&request_id) {
                            let _ = tx.send(Ok(response));
                        }
                    }
                    SwarmEvent::Behaviour(ClientBehaviourEvent::Rendezvous(
                        request_response::Event::OutboundFailure { request_id, error, .. },
                    )) => {
                        if let Some(tx) = pending.lock().await.remove(&request_id) {
                            let _ = tx.send(Err(ClientError::Server(
                                ErrorCode::Unavailable,
                                format!("outbound failure: {error}"),
                            )));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}
