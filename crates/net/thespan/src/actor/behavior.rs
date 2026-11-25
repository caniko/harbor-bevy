// intentionally oversize: release-gate baseline; split requires scoped semantic extraction.
//! Composed libp2p `NetworkBehaviour`.
//!
//! Combines:
//! - request-response (reliable raw messaging for backward compat)
//! - piying remote (typed actor messaging + Kademlia registry)

// `NetworkBehaviour`'s derive emits an unreachable `return` in its generated
// delegate dispatch, and an item-level allow does not reach derive output, so
// scope the allow to this module rather than the crate.
#![allow(unreachable_code)]

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::prelude::*;
use libp2p::core::Endpoint;
use libp2p::request_response;
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm,
};
use libp2p::{Multiaddr, PeerId};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};

pub static KEEPALIVE_MUTEX_CONTENTION_TOTAL: AtomicU64 = AtomicU64::new(0);
pub static KEEPALIVE_TELEMETRY_WRITE_FRAME: AtomicU64 = AtomicU64::new(0);
const KEEPALIVE_TELEMETRY_LOCK_WAIT: Duration = Duration::from_millis(1);
const KEEPALIVE_TELEMETRY_LOCK_WARN_AFTER: Duration = Duration::from_secs(1);

// ---------------------------------------------------------------------------
// Raw codec for request-response
// ---------------------------------------------------------------------------

/// Codec that carries raw `Vec<u8>` payloads over substreams.
///
/// Uses the stream-per-message model: reads until EOF, writes all bytes.
/// Each request-response exchange gets its own yamux substream.
#[derive(Debug, Clone, Copy)]
pub struct RawCodec {
    max_size: u64,
}

impl Default for RawCodec {
    fn default() -> Self {
        Self {
            max_size: 64 * 1024,
        }
    }
}

#[async_trait]
impl request_response::Codec for RawCodec {
    type Protocol = libp2p::StreamProtocol;
    type Request = Vec<u8>;
    type Response = Vec<u8>;

    async fn read_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_length_prefixed(io, self.max_size).await
    }

    async fn read_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_length_prefixed(io, self.max_size).await
    }

    async fn write_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_length_prefixed(io, &req, self.max_size).await?;
        io.close().await?;
        Ok(())
    }

    async fn write_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_length_prefixed(io, &res, self.max_size).await?;
        io.close().await?;
        Ok(())
    }
}

async fn read_length_prefixed<T>(io: &mut T, max_size: u64) -> io::Result<Vec<u8>>
where
    T: AsyncRead + Unpin + Send,
{
    let mut len_buf = [0u8; 4];
    io.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf) as u64;
    if len > max_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("raw message too large: {len} > {max_size}"),
        ));
    }

    let mut vec = vec![0u8; len as usize];
    io.read_exact(&mut vec).await?;
    Ok(vec)
}

async fn write_length_prefixed<T>(io: &mut T, data: &[u8], max_size: u64) -> io::Result<()>
where
    T: AsyncWrite + Unpin + Send,
{
    if data.len() as u64 > max_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("raw message too large: {} > {max_size}", data.len()),
        ));
    }

    io.write_all(&(data.len() as u32).to_le_bytes()).await?;
    io.write_all(data).await
}

// ---------------------------------------------------------------------------
// Composed NetworkBehaviour
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// KeepAlive behaviour — holds a persistent yamux substream to prevent closure
// ---------------------------------------------------------------------------

use libp2p::core::upgrade::ReadyUpgrade;
use libp2p::swarm::handler::{ConnectionEvent, FullyNegotiatedInbound, FullyNegotiatedOutbound};
use libp2p::swarm::{ConnectionHandlerEvent, Stream, StreamProtocol, SubstreamProtocol};

/// Behaviour that opens and holds a persistent yamux substream on every connection,
/// preventing the swarm from closing "idle" connections.
///
/// The passive `connection_keep_alive() = true` approach doesn't reliably prevent
/// closure due to a race in libp2p's connection poll loop (the handler's poll returns
/// `Pending` with no waker, so the connection task may never re-check `keep_alive`).
/// Holding an active `Stream` keeps `stream_counter.has_no_active_streams() = false`,
/// which unconditionally cancels shutdown in the connection's poll loop.
#[derive(Debug, Clone, Copy)]
pub struct KeepAlivePeerTelemetry {
    pub peer_id: PeerId,
    pub pings_sent: u64,
    pub pings_received: u64,
    pub last_ping_at: Option<Instant>,
    pub idle_window_observations: u64,
}

#[derive(Debug, Clone, Default)]
pub struct KeepAliveTelemetryHandle {
    inner: Arc<Mutex<HashMap<PeerId, KeepAlivePeerTelemetry>>>,
}

impl KeepAliveTelemetryHandle {
    pub fn snapshot(&self) -> Vec<KeepAlivePeerTelemetry> {
        let Some(inner) = self.try_telemetry_lock(None, "snapshot") else {
            return Vec::new();
        };
        inner.values().copied().collect()
    }

    pub fn remove_peer(&self, peer_id: PeerId) {
        let Some(mut inner) = self.try_telemetry_lock(Some(peer_id), "remove_peer") else {
            return;
        };
        inner.remove(&peer_id);
    }

    pub fn record_ping_sent(&self, peer_id: PeerId) {
        let now = Instant::now();
        let Some(mut inner) = self.try_telemetry_lock(Some(peer_id), "record_ping_sent") else {
            return;
        };
        let peer = inner.entry(peer_id).or_insert(KeepAlivePeerTelemetry {
            peer_id,
            pings_sent: 0,
            pings_received: 0,
            last_ping_at: None,
            idle_window_observations: 0,
        });
        peer.pings_sent = peer.pings_sent.saturating_add(1);
        peer.last_ping_at = Some(now);
    }

    pub fn record_ping_received(&self, peer_id: PeerId) {
        let now = Instant::now();
        let Some(mut inner) = self.try_telemetry_lock(Some(peer_id), "record_ping_received") else {
            return;
        };
        let peer = inner.entry(peer_id).or_insert(KeepAlivePeerTelemetry {
            peer_id,
            pings_sent: 0,
            pings_received: 0,
            last_ping_at: None,
            idle_window_observations: 0,
        });
        peer.pings_received = peer.pings_received.saturating_add(1);
        peer.last_ping_at = Some(now);
    }

    pub fn record_ping_round_trip(&self, peer_id: PeerId) {
        let now = Instant::now();
        let Some(mut inner) = self.try_telemetry_lock(Some(peer_id), "record_ping_round_trip")
        else {
            return;
        };
        let peer = inner.entry(peer_id).or_insert(KeepAlivePeerTelemetry {
            peer_id,
            pings_sent: 0,
            pings_received: 0,
            last_ping_at: None,
            idle_window_observations: 0,
        });
        peer.pings_sent = peer.pings_sent.saturating_add(1);
        peer.pings_received = peer.pings_received.saturating_add(1);
        peer.last_ping_at = Some(now);
    }

    pub fn observe_idle_window(&self, peer_id: PeerId) {
        let Some(mut inner) = self.try_telemetry_lock(Some(peer_id), "observe_idle_window") else {
            return;
        };
        let peer = inner.entry(peer_id).or_insert(KeepAlivePeerTelemetry {
            peer_id,
            pings_sent: 0,
            pings_received: 0,
            last_ping_at: None,
            idle_window_observations: 0,
        });
        peer.idle_window_observations = peer.idle_window_observations.saturating_add(1);
    }

    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub fn with_lock_for_test(&self, f: impl FnOnce()) {
        let _guard = self.inner.lock();
        f();
    }

    fn try_telemetry_lock(
        &self,
        peer_id: Option<PeerId>,
        operation: &'static str,
    ) -> Option<parking_lot::MutexGuard<'_, HashMap<PeerId, KeepAlivePeerTelemetry>>> {
        let frame = KEEPALIVE_TELEMETRY_WRITE_FRAME.fetch_add(1, Ordering::Relaxed);
        let peer = peer_id
            .map(|peer_id| peer_id.to_string())
            .unwrap_or_else(|| "<all>".to_string());
        tracing::trace!(
            target: "keepalive::write",
            frame,
            peer = %peer,
            operation,
            "write_start"
        );

        let start = Instant::now();
        let guard = self.inner.try_lock_for(KEEPALIVE_TELEMETRY_LOCK_WAIT);
        let elapsed = start.elapsed();
        if guard.is_none() {
            KEEPALIVE_MUTEX_CONTENTION_TOTAL.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                target: "keepalive::write",
                frame,
                peer = %peer,
                operation,
                elapsed_ms = elapsed.as_millis(),
                "write_contention"
            );
        } else if elapsed >= KEEPALIVE_TELEMETRY_LOCK_WARN_AFTER {
            tracing::warn!(
                target: "keepalive::write",
                frame,
                peer = %peer,
                operation,
                elapsed_ms = elapsed.as_millis(),
                "write_slow"
            );
        } else {
            tracing::trace!(
                target: "keepalive::write",
                frame,
                peer = %peer,
                operation,
                elapsed_ms = elapsed.as_millis(),
                "write_done"
            );
        }
        guard
    }
}

pub struct KeepAliveBehaviour {
    telemetry: KeepAliveTelemetryHandle,
    protocol: StreamProtocol,
}

impl Default for KeepAliveBehaviour {
    fn default() -> Self {
        Self::new(KeepAliveTelemetryHandle::default())
    }
}

impl KeepAliveBehaviour {
    pub fn new(telemetry: KeepAliveTelemetryHandle) -> Self {
        Self {
            telemetry,
            protocol: crate::session::stream_protocol(
                &crate::ThespanConfig::default().keepalive_protocol(),
            ),
        }
    }

    pub fn telemetry(&self) -> KeepAliveTelemetryHandle {
        self.telemetry.clone()
    }
}

/// Handler that opens one outbound substream and accepts one inbound substream,
/// holding both for the connection's lifetime to keep the yamux stream counter nonzero.
pub struct KeepAliveHandler {
    pub peer_id: PeerId,
    outbound_requested: bool,
    _outbound_stream: Option<Stream>,
    _inbound_stream: Option<Stream>,
    pub pings_sent: u64,
    pub pings_received: u64,
    pub last_ping_at: Option<Instant>,
    pub idle_window_observations: u64,
    telemetry: KeepAliveTelemetryHandle,
    protocol: StreamProtocol,
}

impl KeepAliveHandler {
    fn new(peer_id: PeerId, behaviour: &KeepAliveBehaviour) -> Self {
        Self {
            peer_id,
            outbound_requested: false,
            _outbound_stream: None,
            _inbound_stream: None,
            pings_sent: 0,
            pings_received: 0,
            last_ping_at: None,
            idle_window_observations: 0,
            telemetry: behaviour.telemetry.clone(),
            protocol: behaviour.protocol.clone(),
        }
    }

    fn record_ping_sent(&mut self) {
        self.pings_sent = self.pings_sent.saturating_add(1);
        self.last_ping_at = Some(Instant::now());
        self.telemetry.record_ping_sent(self.peer_id);
    }

    fn record_ping_received(&mut self) {
        self.pings_received = self.pings_received.saturating_add(1);
        self.last_ping_at = Some(Instant::now());
        self.telemetry.record_ping_received(self.peer_id);
    }

    fn observe_idle_window(&mut self) {
        self.idle_window_observations = self.idle_window_observations.saturating_add(1);
        self.telemetry.observe_idle_window(self.peer_id);
    }
}

impl NetworkBehaviour for KeepAliveBehaviour {
    type ConnectionHandler = KeepAliveHandler;
    type ToSwarm = void::Void;

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        peer_id: PeerId,
        _: &Multiaddr,
        _: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(KeepAliveHandler::new(peer_id, self))
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        peer_id: PeerId,
        _: &Multiaddr,
        _: Endpoint,
        _: libp2p::core::transport::PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(KeepAliveHandler::new(peer_id, self))
    }

    fn on_swarm_event(&mut self, _: FromSwarm) {}

    fn on_connection_handler_event(
        &mut self,
        _: PeerId,
        _: ConnectionId,
        _: THandlerOutEvent<Self>,
    ) {
    }

    fn poll(&mut self, _: &mut Context<'_>) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        Poll::Pending
    }
}

impl libp2p::swarm::ConnectionHandler for KeepAliveHandler {
    type FromBehaviour = void::Void;
    type ToBehaviour = void::Void;
    type InboundProtocol = ReadyUpgrade<StreamProtocol>;
    type OutboundProtocol = ReadyUpgrade<StreamProtocol>;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(ReadyUpgrade::new(self.protocol.clone()), ())
    }

    fn connection_keep_alive(&self) -> bool {
        true
    }

    fn poll(
        &mut self,
        _: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        if !self.outbound_requested {
            self.outbound_requested = true;
            self.record_ping_sent();
            return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                protocol: self.listen_protocol(),
            });
        }
        Poll::Pending
    }

    fn on_behaviour_event(&mut self, v: Self::FromBehaviour) {
        void::unreachable(v)
    }

    fn on_connection_event(
        &mut self,
        event: ConnectionEvent<
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
        match event {
            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: stream,
                ..
            }) => {
                // Hold the stream — its presence in the yamux stream counter
                // prevents the connection from being considered idle.
                // Do NOT call stream.ignore_for_keep_alive().
                tracing::trace!(
                    target: "transport::keepalive",
                    "outbound substream negotiated; holding stream"
                );
                self.record_ping_received();
                self.observe_idle_window();
                self._outbound_stream = Some(stream);
            }
            ConnectionEvent::FullyNegotiatedInbound(FullyNegotiatedInbound {
                protocol: stream,
                ..
            }) => {
                tracing::trace!(
                    target: "transport::keepalive",
                    "inbound substream negotiated; holding stream"
                );
                self.record_ping_received();
                self.observe_idle_window();
                self._inbound_stream = Some(stream);
            }
            ConnectionEvent::DialUpgradeError(e) => {
                // Don't retry: leaving `outbound_requested = true` keeps the
                // poll loop quiescent.
                //
                // We previously reset `outbound_requested = false` here to
                // re-issue the request, but that interacts poorly with
                // libp2p-swarm 0.47.1's `SubstreamRequested` futures: if the
                // previous request's extraction races the retry's poll cycle,
                // the swarm panics with "cannot extract twice"
                // (libp2p-swarm-0.47.1/src/connection.rs:708). The TCP-level
                // keepalive on the underlying yamux substream is sufficient
                // to keep the connection alive even if this protocol's
                // bookkeeping substream never opens.
                tracing::trace!(
                    target: "transport::keepalive",
                    error = ?e.error,
                    "dial upgrade error; not retrying"
                );
            }
            _ => {}
        }
    }
}

/// The composed libp2p `NetworkBehaviour`.
///
/// - `request_response`: Reliable raw message exchange
/// - `piying_remote`: Typed remote actor messaging + Kademlia registry (rkyv codec)
/// - `gossipsub`: Topic-based pub/sub
/// - `ping`: Periodic keep-alive pings to prevent NAT/firewall connection drops
/// - `keep_alive`: Prevents swarm from closing idle connections
/// - `relay_client`: Circuit Relay v2 *client* — used by the internet transport
///   to dial peers behind NAT through a public relay hop. `Toggle::None` for
///   LAN / Steam / WebRTC transports.
/// - `identify`: libp2p identify exchange. Required for the internet relay
///   client to learn relay HOP support before circuit dials.
/// - `dcutr`: Direct Connection Upgrade through Relay (NAT hole punching).
///   Once a peer has been reached via relay, dcutr negotiates a direct path
///   that bypasses the relay. `Toggle::None` for transports where relays
///   are not in use.
// The `NetworkBehaviour` derive emits an unreachable `return` in its generated
// delegate dispatch; the module-level allow above covers it.
#[derive(NetworkBehaviour)]
pub struct ThespanBehaviour {
    pub request_response: request_response::Behaviour<RawCodec>,
    pub piying_remote: piying::remote::Behaviour<piying::remote::codec::PiyingRkyvCodec>,
    pub gossipsub: Toggle<libp2p::gossipsub::Behaviour>,
    pub ping: libp2p::ping::Behaviour,
    pub keep_alive: KeepAliveBehaviour,
    pub relay_client: Toggle<libp2p::relay::client::Behaviour>,
    pub identify: Toggle<libp2p::identify::Behaviour>,
    pub dcutr: Toggle<libp2p::dcutr::Behaviour>,
}

pub(crate) fn build_behaviour(
    key: &libp2p::identity::Keypair,
    protocol: StreamProtocol,
    thespan_config: &crate::ThespanConfig,
    gossipsub_enabled: bool,
    ping_config: libp2p::ping::Config,
    relay_client: Toggle<libp2p::relay::client::Behaviour>,
    identify: Toggle<libp2p::identify::Behaviour>,
    dcutr: Toggle<libp2p::dcutr::Behaviour>,
) -> Result<ThespanBehaviour, Box<dyn std::error::Error + Send + Sync>> {
    let request_response = request_response::Behaviour::<RawCodec>::new(
        [(protocol, request_response::ProtocolSupport::Full)],
        request_response::Config::default().with_request_timeout(Duration::from_secs(30)),
    );

    let messaging_config =
        piying::remote::messaging::Config::default().with_request_timeout(Duration::from_secs(30));
    let codec = piying::remote::codec::PiyingRkyvCodec::new(&messaging_config);
    let piying_remote =
        piying::remote::Behaviour::with_codec(key.public().to_peer_id(), messaging_config, codec);

    let gossipsub = if gossipsub_enabled {
        let config = libp2p::gossipsub::ConfigBuilder::default()
            .mesh_n_low(1)
            .mesh_n(1)
            .mesh_n_high(2)
            .prune_backoff(Duration::from_secs(3600))
            .heartbeat_interval(Duration::from_secs(1))
            .validation_mode(libp2p::gossipsub::ValidationMode::Strict)
            .max_transmit_size(crate::transport::GOSSIPSUB_MAX_TRANSMIT_SIZE)
            .message_id_fn(|message: &libp2p::gossipsub::Message| {
                let mut hasher = Sha256::new();
                hasher.update(&message.data);
                hasher.update(message.topic.as_str().as_bytes());
                libp2p::gossipsub::MessageId::from(hasher.finalize()[..20].to_vec())
            })
            .build()
            .map_err(|error| io::Error::other(format!("invalid GossipSub config: {error}")))?;
        Toggle::from(Some(
            libp2p::gossipsub::Behaviour::new(
                libp2p::gossipsub::MessageAuthenticity::Signed(key.clone()),
                config,
            )
            .map_err(|error| io::Error::other(format!("invalid GossipSub behaviour: {error}")))?,
        ))
    } else {
        Toggle::from(None)
    };

    Ok(ThespanBehaviour {
        request_response,
        piying_remote,
        gossipsub,
        ping: libp2p::ping::Behaviour::new(ping_config),
        keep_alive: KeepAliveBehaviour {
            telemetry: KeepAliveTelemetryHandle::default(),
            protocol: StreamProtocol::try_from_owned(crate::session::applied_protocol(
                &thespan_config.keepalive_protocol(),
            ))?,
        },
        relay_client,
        identify,
        dcutr,
    })
}
