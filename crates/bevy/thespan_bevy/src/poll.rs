//! System that drains the actor event channel each frame and dispatches to the ECS.

use bevy::prelude::*;

use thespan::ActorEvent;

use crate::resource::{
    GossipSubscribeFailedEvent, GossipSubscribedEvent, KeepAliveTelemetry, ListenStartedEvent,
    NetworkResource, OversizedMessageDrops, PeerConnectedEvent, PeerDisconnectedEvent,
    RawGossipMessageEvent, RawMessageEvent, TransportConnectionStatus,
};

/// Drains all pending [`ActorEvent`]s from the network actor and dispatches them.
///
/// Runs in `PreUpdate` so network events are available before application systems.
/// Gated by `run_if(resource_exists::<NetworkResource>)`.
///
/// Typed remote actor messages are dispatched separately by piying.
pub fn poll_network_events(
    mut net: ResMut<NetworkResource>,
    mut keep_alive: ResMut<KeepAliveTelemetry>,
    mut oversized_drops: ResMut<OversizedMessageDrops>,
    mut commands: Commands,
) {
    while let Ok(event) = net.event_rx.try_recv() {
        match event {
            ActorEvent::MessageReceived { from, payload } => {
                debug!("Received raw message ({} bytes) from {from}", payload.len());
                commands.trigger(RawMessageEvent { from, payload });
            }
            ActorEvent::PeerConnected { peer_id, addr } => {
                debug!("Peer connected: {peer_id} at {addr}");
                net.peers.insert(peer_id);
                net.peer_addrs.insert(peer_id, addr.clone());
                net.state = TransportConnectionStatus::Connected;
                commands.trigger(PeerConnectedEvent { peer_id, addr });
            }
            ActorEvent::PeerDisconnected { peer_id, reason } => {
                debug!("Peer disconnected: {peer_id} ({reason})");
                net.peers.remove(&peer_id);
                net.peer_addrs.remove(&peer_id);
                keep_alive.peers.remove(&peer_id);
                net.keep_alive_telemetry.remove_peer(peer_id);
                if net.peers.is_empty() {
                    net.state = TransportConnectionStatus::Disconnected;
                }
                commands.trigger(PeerDisconnectedEvent { peer_id, reason });
            }
            ActorEvent::ListenStarted { addr } => {
                debug!("Listening on {addr}");
                let addr_str = addr.to_string();
                if addr_str.contains("/webrtc-direct") {
                    info!("WEBRTC_JOIN_ADDR={addr}");
                }
                // Add the local PeerId to the relay reservation address so
                // peers that cannot dial directly can use it as-is.
                if addr_str.contains("/p2p-circuit") && !addr_str.contains("/p2p-circuit/p2p/") {
                    info!("INTERNET_JOIN_ADDR={addr}/p2p/{}", net.local_peer_id);
                }
                // The internet transport opens several listeners (QUIC v4/v6,
                // TCP v4/v6, relay circuit). Keep the primary IPv4 QUIC address
                // as the canonical `listen_addr` (the direct dial target) rather
                // than letting a later TCP/IPv6 listener clobber it. Single-
                // listener transports (LAN/WebRTC/Steam) are unaffected.
                let is_primary_quic_v4 = addr_str.contains("/ip4/")
                    && addr_str.contains("/udp/")
                    && addr_str.contains("/quic-v1");
                if net.listen_addr.is_none() || is_primary_quic_v4 {
                    net.listen_addr = Some(addr.clone());
                    if is_primary_quic_v4 {
                        // Advertise the local PeerId on the primary listener so a
                        // relay-only joiner can construct the circuit dial address
                        // `<relay-bootstrap>/p2p-circuit/p2p/<this-peer>`. The
                        // client-side circuit `NewListenAddr` does not reliably surface.
                        info!("INTERNET_PEER_ID={}", net.local_peer_id);
                    }
                }
                commands.trigger(ListenStartedEvent { addr });
            }
            ActorEvent::Error { kind, error } => {
                error!(?kind, "Network actor error: {error}");
            }
            ActorEvent::OversizedMessageDropped {
                peer_id,
                kind,
                observed_bytes,
                limit,
                total_for_peer,
            } => {
                oversized_drops.record(peer_id, total_for_peer);
                warn!(
                    target: "transport::resource",
                    "metric=oversized_message_drops_total peer={peer_id} kind={kind} observed_bytes={observed_bytes} limit={limit} total_for_peer={total_for_peer} Oversized inbound message dropped"
                );
            }
            ActorEvent::GossipReceived {
                topic,
                source,
                payload,
            } => {
                debug!(
                    "GossipSub message on topic {topic} from {source:?} ({} bytes)",
                    payload.len()
                );
                commands.trigger(RawGossipMessageEvent {
                    topic,
                    source,
                    payload,
                });
            }
            ActorEvent::GossipSubscribed { topic } => {
                debug!("Subscribed to GossipSub topic {topic}");
                commands.trigger(GossipSubscribedEvent { topic });
            }
            ActorEvent::GossipSubscribeFailed { topic, reason } => {
                warn!("GossipSub subscribe failed for topic {topic}: {reason}");
                commands.trigger(GossipSubscribeFailedEvent { topic, reason });
            }
        }
    }
}

pub fn sync_keep_alive_telemetry(
    net: Res<NetworkResource>,
    mut keep_alive: ResMut<KeepAliveTelemetry>,
) {
    keep_alive.sync_from_transport(net.keep_alive_telemetry.snapshot());
}

pub fn clear_keep_alive_telemetry(mut keep_alive: ResMut<KeepAliveTelemetry>) {
    if !keep_alive.peers.is_empty() {
        keep_alive.peers.clear();
    }
}
