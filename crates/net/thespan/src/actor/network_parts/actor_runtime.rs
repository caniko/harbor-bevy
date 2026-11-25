// intentionally oversize: cohesive order-sensitive builder or driver flow; split requires semantic extraction.
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures::StreamExt;
use libp2p::multiaddr::Protocol;
use libp2p::request_response;
use libp2p::swarm::SwarmEvent;
use libp2p::swarm::dial_opts::{DialOpts, PeerCondition};
use libp2p::{Multiaddr, PeerId, Swarm};
use log::{debug, error, info, trace, warn};
use piying::Actor;
use piying::actor::{ActorRef, WeakActorRef};
use piying::error::ActorStopReason;
use piying::mailbox::{MailboxReceiver, Signal};
use piying::message::{Context, Message};

use crate::actor::behavior::{KeepAliveTelemetryHandle, ThespanBehaviour, ThespanBehaviourEvent};
use crate::actor::commands::{NetworkCommand, SendTarget};
use crate::actor::events::{ActorErrorKind, ActorEvent};
use crate::handle::THESPAN_EVENT_DROPS;
use crate::spawn::THESPAN_EVENT_CHANNEL_CAPACITY;
use crate::transport::{TransportError, TransportMessageKind};

pub type NetworkBootstrapSender = std::sync::mpsc::SyncSender<
    Result<(piying::remote::SwarmSender, KeepAliveTelemetryHandle), TransportError>,
>;

/// Well-known marker payload sent during graceful disconnect.
/// Recognized by the receiver to distinguish intentional disconnects from drops.
pub const BYE_MARKER: &[u8] = b"BYE";
const INTERNET_RELAY_READY_TIMEOUT: Duration = Duration::from_secs(15);
const INTERNET_DIAL_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(15);

/// Arguments for initializing the network actor.
///
/// The caller builds the `Swarm<ThespanBehaviour>` with their desired transport
/// and passes it here along with the address to listen on.
pub struct NetworkActorArgs {
    pub swarm: Swarm<ThespanBehaviour>,
    pub listen_addr: libp2p::Multiaddr,
    pub additional_listen_addrs: Vec<libp2p::Multiaddr>,
    pub event_tx: crossbeam_channel::Sender<ActorEvent>,
    pub bootstrap_tx: Option<NetworkBootstrapSender>,
    /// Transport infrastructure peers that must not surface as application peers.
    pub infrastructure_peers: HashSet<PeerId>,
    /// Optional address to dial immediately after listening.
    /// Used when the remote peer is known upfront.
    pub dial_addr: Option<libp2p::Multiaddr>,
}

/// The main network actor. Owns the libp2p Swarm, receives commands via piying mailbox,
/// and pushes events to the consumer via crossbeam channel.
pub struct NetworkActor {
    swarm: Swarm<ThespanBehaviour>,
    event_tx: crossbeam_channel::Sender<ActorEvent>,
    /// All currently connected peers.
    connected_peers: HashSet<PeerId>,
    /// Connected transport infrastructure peers, such as Circuit Relay servers.
    infrastructure_peers: HashSet<PeerId>,
    /// Peers for which libp2p identify has completed.
    identified_peers: HashSet<PeerId>,
    oversized_message_drops: HashMap<PeerId, u64>,
}

impl Actor for NetworkActor {
    type Args = NetworkActorArgs;
    type Error = TransportError;

    async fn on_start(args: Self::Args, _actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        let mut swarm = args.swarm;

        // Initialize piying's global ActorSwarm so remote actors can send/receive.
        // This must happen before any RemoteActor operations.
        if let Err(e) = swarm.behaviour().piying_remote.try_init_global() {
            warn!("piying ActorSwarm already initialized: {e}");
        }

        if let Err(e) = swarm.listen_on(args.listen_addr.clone()) {
            let err = TransportError::Listen {
                addr: args.listen_addr,
                message: e.to_string(),
            };
            let _ = args.event_tx.try_send(ActorEvent::Error {
                kind: ActorErrorKind::ListenFailed,
                error: err.to_string(),
            });
            if let Some(bootstrap_tx) = args.bootstrap_tx {
                let _ = bootstrap_tx.send(Err(TransportError::Listen {
                    addr: match &err {
                        TransportError::Listen { addr, .. } => addr.clone(),
                        _ => unreachable!(),
                    },
                    message: match &err {
                        TransportError::Listen { message, .. } => message.clone(),
                        _ => unreachable!(),
                    },
                }));
            }
            return Err(err);
        }

        for addr in args.additional_listen_addrs {
            match swarm.listen_on(addr.clone()) {
                Ok(_listener_id) => {
                    info!("Listening on additional address {addr}");
                }
                Err(e) => {
                    let err = TransportError::Listen {
                        addr,
                        message: e.to_string(),
                    };
                    warn!("additional listen address failed: {err}");
                    let _ = args.event_tx.try_send(ActorEvent::Error {
                        kind: ActorErrorKind::ListenFailed,
                        error: err.to_string(),
                    });
                }
            }
        }

        if let Some(bootstrap_tx) = args.bootstrap_tx {
            let swarm_sender = swarm.behaviour().piying_remote.swarm_sender();
            let keep_alive_telemetry = swarm.behaviour().keep_alive.telemetry();
            let _ = bootstrap_tx.send(Ok((swarm_sender, keep_alive_telemetry)));
        }

        let mut actor = NetworkActor {
            swarm,
            event_tx: args.event_tx,
            connected_peers: HashSet::new(),
            infrastructure_peers: args.infrastructure_peers,
            identified_peers: HashSet::new(),
            oversized_message_drops: HashMap::new(),
        };

        // Auto-dial remote peer if specified (Steam transport and internet join).
        if let Some(dial_addr) = args.dial_addr {
            actor.dial_startup_addr(dial_addr).await;
        }

        info!(
            "NetworkActor started, local peer: {}",
            actor.swarm.local_peer_id()
        );

        Ok(actor)
    }

    fn on_stop(
        &mut self,
        _actor_ref: WeakActorRef<Self>,
        reason: ActorStopReason,
    ) -> impl Future<Output = Result<(), Self::Error>> {
        info!("NetworkActor shutting down: {reason:?}");
        let peers: Vec<PeerId> = self.connected_peers.drain().collect();
        for pid in peers {
            let _ = self.swarm.disconnect_peer_id(pid);
        }
        std::future::ready(Ok(()))
    }

    /// Poll both the piying mailbox and the libp2p swarm.
    /// Swarm events are handled inline; mailbox signals are returned for piying dispatch.
    async fn next(
        &mut self,
        _actor_ref: WeakActorRef<Self>,
        mailbox_rx: &mut MailboxReceiver<Self>,
    ) -> Option<Signal<Self>> {
        loop {
            tokio::select! {
                signal = mailbox_rx.recv() => {
                    return signal;
                }
                event = self.swarm.select_next_some() => {
                    self.handle_swarm_event(event);
                }
            }
        }
    }
}

fn peer_id_from_addr(addr: &Multiaddr) -> Option<PeerId> {
    match addr.iter().last() {
        Some(Protocol::P2p(peer_id)) => Some(peer_id),
        _ => None,
    }
}

fn is_circuit_addr(addr: &Multiaddr) -> bool {
    addr.iter()
        .any(|protocol| matches!(protocol, Protocol::P2pCircuit))
}

fn circuit_relay_addr(circuit_addr: &Multiaddr) -> Option<(PeerId, Multiaddr)> {
    let mut relay_addr = Multiaddr::empty();
    let mut relay_peer = None;

    for protocol in circuit_addr.iter() {
        if matches!(protocol, Protocol::P2pCircuit) {
            return relay_peer.map(|peer_id| (peer_id, relay_addr));
        }
        if let Protocol::P2p(peer_id) = protocol {
            relay_peer = Some(peer_id);
        }
        relay_addr.push(protocol);
    }

    None
}

/// `NetworkCommand` is the single message type for the actor.
/// Each variant is dispatched to the appropriate handler.
impl Message<NetworkCommand> for NetworkActor {
    type Reply = ();

    async fn handle(&mut self, cmd: NetworkCommand, _ctx: &mut Context<Self, Self::Reply>) {
        match cmd {
            NetworkCommand::Listen { addr } => match self.swarm.listen_on(addr.clone()) {
                Ok(_listener_id) => {
                    info!("Listening on {addr}");
                }
                Err(e) => {
                    let err = TransportError::Listen {
                        addr: addr.clone(),
                        message: e.to_string(),
                    };
                    error!("transport error: {err}");
                    self.push_event(ActorEvent::Error {
                        kind: ActorErrorKind::ListenFailed,
                        error: err.to_string(),
                    });
                }
            },
            NetworkCommand::Dial { peer_id, addr } => {
                if let Err(err) = self.dial_peer_with_retry(peer_id, addr).await {
                    warn!("transport error: {err}");
                    self.push_event(ActorEvent::Error {
                        kind: ActorErrorKind::DialFailed,
                        error: err.to_string(),
                    });
                }
            }
            NetworkCommand::SendMessage { payload, target } => {
                if payload.len() > 1200 {
                    warn!(
                        "Sending large payload ({} bytes) — may cause fragmentation",
                        payload.len()
                    );
                }
                let targets: Vec<PeerId> = match target {
                    SendTarget::Peer(pid) => {
                        if self.connected_peers.contains(&pid) {
                            vec![pid]
                        } else {
                            warn!("Cannot send to {pid}: not connected");
                            vec![]
                        }
                    }
                    SendTarget::All => self.connected_peers.iter().copied().collect(),
                };
                if targets.is_empty() && self.connected_peers.is_empty() {
                    warn!("Cannot send message: no connected peers");
                }
                // Clone the payload for every target except the last, into which we
                // move the owned bytes (dominant P2P 1v1 case sends to a single peer,
                // so this avoids a full payload copy per outbound message).
                let mut payload = payload;
                let mut targets = targets.into_iter().peekable();
                while let Some(peer_id) = targets.next() {
                    let body = if targets.peek().is_some() {
                        payload.clone()
                    } else {
                        std::mem::take(&mut payload)
                    };
                    let _request_id = self
                        .swarm
                        .behaviour_mut()
                        .request_response
                        .send_request(&peer_id, body);
                    debug!("Sent message to {peer_id}");
                }
            }
            NetworkCommand::Disconnect { peer_id } => match peer_id {
                Some(pid) => {
                    if self.connected_peers.remove(&pid) {
                        info!("Disconnecting from peer {pid}");
                        let _ = self.swarm.disconnect_peer_id(pid);
                    }
                }
                None => {
                    let peers: Vec<PeerId> = self.connected_peers.drain().collect();
                    for pid in peers {
                        info!("Disconnecting from peer {pid}");
                        let _ = self.swarm.disconnect_peer_id(pid);
                    }
                }
            },
            NetworkCommand::GracefulDisconnect { peer_id } => {
                let targets: Vec<PeerId> = match peer_id {
                    Some(pid) => {
                        self.connected_peers.remove(&pid);
                        vec![pid]
                    }
                    None => self.connected_peers.drain().collect(),
                };
                for pid in targets {
                    info!("Gracefully disconnecting from peer {pid}");
                    let _ = self
                        .swarm
                        .behaviour_mut()
                        .request_response
                        .send_request(&pid, BYE_MARKER.to_vec());
                    let _ = self.swarm.disconnect_peer_id(pid);
                }
            }
            NetworkCommand::Shutdown => {
                // piying handles shutdown via actor_ref.stop_gracefully().
                // This variant is kept for the Steam transport path which
                // still uses raw channels.
                info!("Received Shutdown command (no-op for piying actor)");
            }
            NetworkCommand::Subscribe { topic } => {
                if let Some(gs) = self.swarm.behaviour_mut().gossipsub.as_mut() {
                    let topic_hash = crate::session::gossipsub_topic(&topic);
                    match gs.subscribe(&topic_hash) {
                        Ok(true) => {
                            info!("Subscribed to GossipSub topic: {topic}");
                            self.push_event(ActorEvent::GossipSubscribed { topic });
                        }
                        Ok(false) => {
                            debug!("Already subscribed to GossipSub topic: {topic}");
                            self.push_event(ActorEvent::GossipSubscribed { topic });
                        }
                        Err(e) => {
                            let reason = e.to_string();
                            error!("Failed to subscribe to topic {topic}: {reason}");
                            self.push_event(ActorEvent::GossipSubscribeFailed { topic, reason });
                        }
                    }
                } else {
                    let reason = "GossipSub not enabled".to_string();
                    warn!("{reason}, ignoring Subscribe");
                    self.push_event(ActorEvent::GossipSubscribeFailed { topic, reason });
                }
            }
            NetworkCommand::Unsubscribe { topic } => {
                if let Some(gs) = self.swarm.behaviour_mut().gossipsub.as_mut() {
                    let topic_hash = crate::session::gossipsub_topic(&topic);
                    if gs.unsubscribe(&topic_hash) {
                        info!("Unsubscribed from GossipSub topic: {topic}");
                    } else {
                        debug!("Was not subscribed to GossipSub topic: {topic}");
                    }
                } else {
                    warn!("GossipSub not enabled, ignoring Unsubscribe");
                }
            }
            NetworkCommand::PublishGossip { topic, payload } => {
                if let Some(gs) = self.swarm.behaviour_mut().gossipsub.as_mut() {
                    let topic_hash = crate::session::gossipsub_topic(&topic);
                    match gs.publish(topic_hash, payload) {
                        Ok(msg_id) => debug!("Published to GossipSub topic {topic}: {msg_id}"),
                        Err(e) => error!("Failed to publish to topic {topic}: {e}"),
                    }
                } else {
                    warn!("GossipSub not enabled, ignoring PublishGossip");
                }
            }
        }
    }
}

impl NetworkActor {
    fn handle_swarm_event(&mut self, event: SwarmEvent<ThespanBehaviourEvent>) {
        match event {
            // --- Connection events ---
            // libp2p can establish multiple connections to the same peer
            // (e.g. simultaneous direct dials). Only fire PeerConnected
            // for the FIRST connection and PeerDisconnected when ALL close.
            SwarmEvent::ConnectionEstablished {
                peer_id,
                num_established,
                endpoint,
                established_in,
                concurrent_dial_errors,
                ..
            } => {
                self.connected_peers.insert(peer_id);
                if let Some(errors) = concurrent_dial_errors {
                    for (addr, err) in errors {
                        trace!(
                            target: "transport::dial",
                            "Concurrent dial candidate failed peer={peer_id} addr={addr}: {err}"
                        );
                    }
                }
                trace!(
                    target: "transport::dial",
                    "ConnectionEstablished peer={peer_id} total={num_established} endpoint={}",
                    endpoint.get_remote_address()
                );
                if num_established.get() == 1 {
                    let addr = endpoint.get_remote_address().clone();
                    if self.infrastructure_peers.contains(&peer_id) {
                        info!(
                            target: "transport::dial",
                            "Infrastructure connection established peer={peer_id} addr={addr} established_ms={}",
                            established_in.as_millis()
                        );
                        return;
                    }
                    info!(
                        target: "transport::dial",
                        "Connection established peer={peer_id} addr={addr} established_ms={}",
                        established_in.as_millis()
                    );
                    self.push_event(ActorEvent::PeerConnected { peer_id, addr });
                }
            }
            SwarmEvent::ConnectionClosed {
                peer_id,
                num_established: 0,
                cause,
                ..
            } => {
                trace!(
                    target: "transport::dial",
                    "ConnectionClosed peer={peer_id} remaining=0 cause={cause:?}"
                );
                let reason = cause
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                self.connected_peers.remove(&peer_id);
                if self.infrastructure_peers.contains(&peer_id) {
                    info!(
                        target: "transport::dial",
                        "Infrastructure connection closed peer={peer_id} reason={reason}"
                    );
                    return;
                }
                self.push_event(ActorEvent::PeerDisconnected { peer_id, reason });
            }
            SwarmEvent::ConnectionClosed {
                peer_id,
                num_established,
                cause,
                ..
            } => {
                trace!(
                    target: "transport::dial",
                    "ConnectionClosed peer={peer_id} remaining={num_established} cause={cause:?}"
                );
            }

            // --- Request-Response events ---
            SwarmEvent::Behaviour(ThespanBehaviourEvent::RequestResponse(rr_event)) => {
                self.handle_request_response(rr_event);
            }

            // --- piying remote events (actor messaging + registry) ---
            // These are handled internally by piying; we just log for diagnostics.
            SwarmEvent::Behaviour(ThespanBehaviourEvent::PiyingRemote(remote_event)) => {
                debug!("piying remote event: {remote_event:?}");
            }

            // --- GossipSub events ---
            SwarmEvent::Behaviour(ThespanBehaviourEvent::Gossipsub(gs_event)) => {
                self.handle_gossipsub(gs_event);
            }

            SwarmEvent::Behaviour(ThespanBehaviourEvent::RelayClient(relay_event)) => {
                info!(target: "transport::nat", "relay client event: {relay_event:?}");
            }

            SwarmEvent::Behaviour(ThespanBehaviourEvent::Identify(identify_event)) => {
                self.handle_identify(identify_event);
            }

            SwarmEvent::Behaviour(ThespanBehaviourEvent::Dcutr(dcutr_event)) => {
                info!(target: "transport::nat", "dcutr event: {dcutr_event:?}");
            }

            // --- Ping events (keep-alive telemetry) ---
            SwarmEvent::Behaviour(ThespanBehaviourEvent::Ping(event)) if event.result.is_ok() => {
                self.swarm
                    .behaviour()
                    .keep_alive
                    .telemetry()
                    .record_ping_round_trip(event.peer);
            }

            // --- KeepAlive events (none expected, void type) ---
            SwarmEvent::Behaviour(ThespanBehaviourEvent::KeepAlive(v)) => {
                void::unreachable(v);
            }

            // --- Listener events ---
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(target: "transport::listener", "Swarm listening on {address}");
                self.push_event(ActorEvent::ListenStarted { addr: address });
            }

            SwarmEvent::ExpiredListenAddr { address, .. } => {
                info!(target: "transport::listener", "Listen address expired: {address}");
            }

            SwarmEvent::ListenerClosed {
                listener_id,
                addresses,
                reason,
            } => {
                info!(
                    target: "transport::listener",
                    "Listener closed id={listener_id:?} addresses={addresses:?} reason={reason:?}"
                );
            }

            SwarmEvent::ListenerError { listener_id, error } => {
                warn!(
                    target: "transport::listener",
                    "Listener error id={listener_id:?}: {error}"
                );
            }

            SwarmEvent::IncomingConnection {
                local_addr,
                send_back_addr,
                ..
            } => {
                trace!(
                    target: "transport::listener",
                    "Incoming connection local_addr={local_addr} send_back_addr={send_back_addr}"
                );
            }

            SwarmEvent::IncomingConnectionError {
                local_addr,
                send_back_addr,
                peer_id,
                error,
                ..
            } => {
                let err = TransportError::Stream {
                    peer_id,
                    message: error.to_string(),
                };
                warn!(
                    target: "transport::listener",
                    "Incoming connection error local_addr={local_addr} send_back_addr={send_back_addr}: {err}"
                );
            }

            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                let err = TransportError::Dial {
                    peer_id,
                    addr: None,
                    attempts: 1,
                    message: error.to_string(),
                };
                warn!(target: "transport::dial", "{err}");
            }

            SwarmEvent::Dialing { peer_id, .. } => {
                trace!(target: "transport::dial", "Dialing peer={peer_id:?}");
            }

            SwarmEvent::NewExternalAddrCandidate { address } => {
                trace!(target: "transport::ice", "External address candidate: {address}");
            }

            SwarmEvent::ExternalAddrConfirmed { address } => {
                info!(target: "transport::ice", "External address confirmed: {address}");
            }

            SwarmEvent::ExternalAddrExpired { address } => {
                info!(target: "transport::ice", "External address expired: {address}");
            }

            SwarmEvent::NewExternalAddrOfPeer { peer_id, address } => {
                trace!(target: "transport::ice", "Peer external address peer={peer_id} addr={address}");
            }

            // Ignore other swarm events
            _ => {}
        }
    }

    fn handle_request_response(&mut self, event: request_response::Event<Vec<u8>, Vec<u8>>) {
        match event {
            request_response::Event::Message { peer, message, .. } => match message {
                request_response::Message::Request {
                    request, channel, ..
                } => {
                    debug!("Received request from {peer} ({} bytes)", request.len());
                    if self.drop_if_oversized_direct(peer, request.len()) {
                        let _ = self
                            .swarm
                            .behaviour_mut()
                            .request_response
                            .send_response(channel, vec![]);
                        return;
                    }
                    // Push the message to consumer
                    self.push_event(ActorEvent::MessageReceived {
                        from: peer,
                        payload: request,
                    });
                    // Send an empty ACK response to complete the request-response exchange
                    let _ = self
                        .swarm
                        .behaviour_mut()
                        .request_response
                        .send_response(channel, vec![]);
                }
                request_response::Message::Response { response, .. } => {
                    // Responses are only acknowledgements.
                    // But if a response contains data, treat it as a message.
                    if !response.is_empty() {
                        if self.drop_if_oversized_direct(peer, response.len()) {
                            return;
                        }
                        self.push_event(ActorEvent::MessageReceived {
                            from: peer,
                            payload: response,
                        });
                    }
                }
            },
            request_response::Event::OutboundFailure { peer, error, .. } => {
                warn!("Outbound request to {peer} failed: {error}");
            }
            request_response::Event::InboundFailure { peer, error, .. } => {
                warn!("Inbound request from {peer} failed: {error}");
            }
            request_response::Event::ResponseSent { .. } => {
                // ACK sent successfully, no action needed.
            }
        }
    }

    fn handle_gossipsub(&mut self, event: libp2p::gossipsub::Event) {
        match event {
            libp2p::gossipsub::Event::Message {
                propagation_source: _,
                message_id: _,
                message,
            } => {
                let topic = message.topic.to_string();
                debug!(
                    "GossipSub message on topic {topic} from {:?} ({} bytes)",
                    message.source,
                    message.data.len()
                );
                self.push_event(ActorEvent::GossipReceived {
                    topic: topic.into(),
                    source: message.source,
                    payload: message.data,
                });
            }
            libp2p::gossipsub::Event::Subscribed { peer_id, topic } => {
                info!("Peer {peer_id} subscribed to topic {topic}");
            }
            libp2p::gossipsub::Event::Unsubscribed { peer_id, topic } => {
                info!("Peer {peer_id} unsubscribed from topic {topic}");
            }
            libp2p::gossipsub::Event::GossipsubNotSupported { peer_id } => {
                debug!("Peer {peer_id} does not support GossipSub");
            }
            _ => {}
        }
    }

    fn handle_identify(&mut self, event: libp2p::identify::Event) {
        match event {
            libp2p::identify::Event::Received { peer_id, info, .. } => {
                self.identified_peers.insert(peer_id);
                info!(
                    target: "transport::nat",
                    "Identify received peer={peer_id} protocols={} listen_addrs={}",
                    info.protocols.len(),
                    info.listen_addrs.len()
                );
            }
            libp2p::identify::Event::Sent { peer_id, .. } => {
                trace!(target: "transport::nat", "Identify sent peer={peer_id}");
            }
            libp2p::identify::Event::Pushed { peer_id, .. } => {
                trace!(target: "transport::nat", "Identify pushed peer={peer_id}");
            }
            libp2p::identify::Event::Error { peer_id, error, .. } => {
                warn!(target: "transport::nat", "Identify failed peer={peer_id}: {error}");
            }
        }
    }

    fn push_event(&self, event: ActorEvent) {
        match self.event_tx.try_send(event) {
            Ok(()) => {}
            Err(crossbeam_channel::TrySendError::Full(_)) => {
                let dropped =
                    THESPAN_EVENT_DROPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if dropped == 1 || dropped.is_multiple_of(1024) {
                    tracing::warn!(
                        target: "thespan::backpressure",
                        capacity = THESPAN_EVENT_CHANNEL_CAPACITY,
                        dropped,
                        "thespan event channel full; dropping event"
                    );
                } else {
                    tracing::debug!(
                        target: "thespan::backpressure",
                        capacity = THESPAN_EVENT_CHANNEL_CAPACITY,
                        dropped,
                        "thespan event channel full; dropping event"
                    );
                }
            }
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                warn!("Failed to push ActorEvent: receiver dropped");
            }
        }
    }

    fn drop_if_oversized_direct(&mut self, peer_id: PeerId, observed_bytes: usize) -> bool {
        let kind = TransportMessageKind::DirectSession;
        let limit = kind.max_bytes();
        if observed_bytes <= limit {
            return false;
        }

        let total_for_peer = {
            let slot = self.oversized_message_drops.entry(peer_id).or_insert(0);
            *slot = slot.saturating_add(1);
            *slot
        };
        let err = TransportError::OversizedMessage {
            peer_id: Some(peer_id),
            kind: kind.label(),
            observed_bytes,
            limit,
        };
        warn!(
            target: "transport::resource",
            "metric=oversized_message_drops_total peer={peer_id} kind={} observed_bytes={observed_bytes} limit={limit} total_for_peer={total_for_peer} {err}",
            kind.label(),
        );
        self.push_event(ActorEvent::OversizedMessageDropped {
            peer_id,
            kind: kind.label(),
            observed_bytes,
            limit,
            total_for_peer,
        });
        true
    }

    async fn dial_startup_addr(&mut self, dial_addr: Multiaddr) {
        info!("Auto-dialing remote peer at {dial_addr}");
        let Some(peer_id) = peer_id_from_addr(&dial_addr) else {
            if let Err(e) = self.swarm.dial(dial_addr.clone()) {
                error!("Failed to auto-dial {dial_addr}: {e}");
            }
            return;
        };

        if let Err(err) = self.dial_peer_with_retry(peer_id, dial_addr).await {
            warn!("transport error: {err}");
            self.push_event(ActorEvent::Error {
                kind: ActorErrorKind::DialFailed,
                error: err.to_string(),
            });
        }
    }

    async fn ensure_circuit_relay_ready(
        &mut self,
        circuit_addr: &Multiaddr,
    ) -> Result<(), TransportError> {
        let Some((relay_peer, relay_addr)) = circuit_relay_addr(circuit_addr) else {
            return Ok(());
        };

        if self.relay_ready(relay_peer) {
            return Ok(());
        }

        if !self.swarm.is_connected(&relay_peer) {
            info!(
                target: "transport::dial",
                "Dialing relay peer {relay_peer} before circuit dial relay_addr={relay_addr}"
            );
            let opts = DialOpts::peer_id(relay_peer)
                .condition(PeerCondition::DisconnectedAndNotDialing)
                .addresses(vec![relay_addr.clone()])
                .build();
            if let Err(err) = self.swarm.dial(opts) {
                let message = err.to_string();
                if !self.swarm.is_connected(&relay_peer) {
                    return Err(TransportError::Dial {
                        peer_id: Some(relay_peer),
                        addr: Some(relay_addr),
                        attempts: 1,
                        message,
                    });
                }
            }
        }

        let timeout = tokio::time::sleep(INTERNET_RELAY_READY_TIMEOUT);
        tokio::pin!(timeout);

        loop {
            if self.relay_ready(relay_peer) {
                info!(
                    target: "transport::dial",
                    "Relay peer {relay_peer} ready for circuit dial"
                );
                return Ok(());
            }

            tokio::select! {
                event = self.swarm.select_next_some() => {
                    let relay_dial_error = match &event {
                        SwarmEvent::OutgoingConnectionError { peer_id, error, .. }
                            if peer_id == &Some(relay_peer) => Some(error.to_string()),
                        _ => None,
                    };
                    self.handle_swarm_event(event);
                    if let Some(message) = relay_dial_error {
                        return Err(TransportError::Dial {
                            peer_id: Some(relay_peer),
                            addr: Some(relay_addr),
                            attempts: 1,
                            message,
                        });
                    }
                }
                _ = &mut timeout => {
                    return Err(TransportError::Dial {
                        peer_id: Some(relay_peer),
                        addr: Some(relay_addr),
                        attempts: 1,
                        message: format!(
                            "timed out after {:?} waiting for relay connection and identify",
                            INTERNET_RELAY_READY_TIMEOUT
                        ),
                    });
                }
            }
        }
    }

    fn relay_ready(&self, relay_peer: PeerId) -> bool {
        self.swarm.is_connected(&relay_peer) && self.identified_peers.contains(&relay_peer)
    }

    fn dial_peer_attempt(
        &mut self,
        peer_id: PeerId,
        addr: &Multiaddr,
        circuit_addr: bool,
    ) -> Result<(), String> {
        if circuit_addr {
            self.swarm.dial(addr.clone()).map_err(|err| err.to_string())
        } else {
            self.swarm.add_peer_address(peer_id, addr.clone());
            self.swarm.dial(peer_id).map_err(|err| err.to_string())
        }
    }

    async fn wait_for_peer_connection(
        &mut self,
        peer_id: PeerId,
        addr: &Multiaddr,
    ) -> Result<(), String> {
        let timeout = tokio::time::sleep(INTERNET_DIAL_ATTEMPT_TIMEOUT);
        tokio::pin!(timeout);

        loop {
            if self.swarm.is_connected(&peer_id) {
                return Ok(());
            }

            tokio::select! {
                event = self.swarm.select_next_some() => {
                    let dial_error = match &event {
                        SwarmEvent::OutgoingConnectionError { peer_id: event_peer_id, error, .. }
                            if event_peer_id == &Some(peer_id) => Some(error.to_string()),
                        _ => None,
                    };
                    self.handle_swarm_event(event);
                    if self.swarm.is_connected(&peer_id) {
                        return Ok(());
                    }
                    if let Some(error) = dial_error {
                        return Err(error);
                    }
                }
                _ = &mut timeout => {
                    return Err(format!(
                        "timed out after {:?} waiting for connection to {peer_id} at {addr}",
                        INTERNET_DIAL_ATTEMPT_TIMEOUT
                    ));
                }
            }
        }
    }

    async fn wait_for_peer_or_delay(&mut self, peer_id: PeerId, delay: Duration) -> bool {
        let timeout = tokio::time::sleep(delay);
        tokio::pin!(timeout);

        loop {
            if self.swarm.is_connected(&peer_id) {
                return true;
            }

            tokio::select! {
                event = self.swarm.select_next_some() => {
                    self.handle_swarm_event(event);
                }
                _ = &mut timeout => {
                    return self.swarm.is_connected(&peer_id);
                }
            }
        }
    }

    async fn dial_peer_with_retry(
        &mut self,
        peer_id: PeerId,
        addr: Multiaddr,
    ) -> Result<(), TransportError> {
        let circuit_addr = is_circuit_addr(&addr);
        let mut delay = crate::transport::libp2p_internet::INTERNET_DIAL_START_DELAY;
        let mut last_error = String::new();
        for attempt in 1..=crate::transport::libp2p_internet::INTERNET_DIAL_MAX_ATTEMPTS {
            if self.swarm.is_connected(&peer_id) {
                return Ok(());
            }

            if circuit_addr {
                self.ensure_circuit_relay_ready(&addr).await?;
            }

            info!(
                target: "transport::dial",
                "Dialing peer {peer_id} at {addr} attempt={attempt}/{}",
                crate::transport::libp2p_internet::INTERNET_DIAL_MAX_ATTEMPTS
            );
            match self.dial_peer_attempt(peer_id, &addr, circuit_addr) {
                Ok(()) => match self.wait_for_peer_connection(peer_id, &addr).await {
                    Ok(()) => return Ok(()),
                    Err(error) => {
                        last_error = error;
                    }
                },
                Err(e) => {
                    last_error = e;
                }
            }

            if attempt == crate::transport::libp2p_internet::INTERNET_DIAL_MAX_ATTEMPTS {
                break;
            }

            let sleep_for = jittered_delay(delay, attempt);
            warn!(
                target: "transport::dial",
                "Dial attempt {attempt}/{} failed peer={peer_id} addr={addr}: {last_error}; retrying in {sleep_for:?}",
                crate::transport::libp2p_internet::INTERNET_DIAL_MAX_ATTEMPTS
            );
            if self.wait_for_peer_or_delay(peer_id, sleep_for).await {
                return Ok(());
            }
            delay = (delay * 2).min(crate::transport::libp2p_internet::INTERNET_DIAL_MAX_DELAY);
        }

        Err(TransportError::Dial {
            peer_id: Some(peer_id),
            addr: Some(addr),
            attempts: crate::transport::libp2p_internet::INTERNET_DIAL_MAX_ATTEMPTS,
            message: last_error,
        })
    }
}
