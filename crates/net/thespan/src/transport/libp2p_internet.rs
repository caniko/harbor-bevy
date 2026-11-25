//! Internet transport: TCP + QUIC, with Circuit Relay v2 client and
//! dcutr (NAT hole punching) enabled.
//!
//! Sibling of [`super::lan`] / [`super::webrtc`] / [`super::steam`]. Same
//! `ThespanBehaviour` actor framework on top, different carrier underneath.
//!
//! Where this transport differs:
//! - **QUIC first, TCP fallback** dual carriers (LAN is QUIC-only; internet
//!   keeps TCP for restrictive networks where UDP is blocked).
//! - **Circuit Relay v2** as a client — tried after direct addresses, for
//!   peers behind symmetric NATs (typical mobile cellular).
//! - **dcutr** — once a peer has been reached through the relay, direct
//!   hole-punching upgrades the connection to peer-to-peer.
//! - Discovery flows through direct dial and the rendezvous service.
//!
//! The caller supplies bootstrap multiaddrs and can trigger dials separately.

use std::collections::HashSet;
use std::time::Duration;

use libp2p::swarm::Swarm;
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::{Multiaddr, PeerId, SwarmBuilder, multiaddr::Protocol};

use crate::actor::behavior::{ThespanBehaviour, build_behaviour};
use crate::config::ThespanConfig;
use crate::transport::{SWARM_IDLE_CONNECTION_TIMEOUT, TransportError};

/// QUIC idle timeout for internet transport, in milliseconds.
pub const INTERNET_QUIC_IDLE_TIMEOUT_MS: u32 = 60_000;
/// Direct/relay dial attempts before the actor reports a dial failure.
pub const INTERNET_DIAL_MAX_ATTEMPTS: u8 = 8;
/// Initial retry delay for WAN dials.
pub const INTERNET_DIAL_START_DELAY: Duration = Duration::from_millis(100);
/// Maximum retry delay for WAN dials.
pub const INTERNET_DIAL_MAX_DELAY: Duration = Duration::from_secs(10);
/// Configuration for the internet transport.
#[derive(Debug, Clone)]
pub struct InternetConfig {
    /// Legacy request-response protocol ID. Must start with `/` when enabled.
    pub protocol: String,
    /// QUIC (UDP) listen port. `0` for OS-assigned.
    pub quic_port: u16,
    /// TCP listen port. `0` for OS-assigned.
    pub tcp_port: u16,
    /// Optional address(es) to dial immediately on startup. Pass the multiaddrs
    /// from a `PeerTicket` obtained via the rendezvous client. The actor
    /// dials the first; the others may be tried by upper layers if needed.
    pub dial_addr: Option<libp2p::Multiaddr>,
    /// Relay server addresses to reserve on startup. Each address must identify
    /// the relay peer, e.g. `/ip4/203.0.113.10/udp/4001/quic-v1/p2p/<relay>`.
    pub relay_reservation_addrs: Vec<libp2p::Multiaddr>,
    /// Enable Circuit Relay v2 client behaviour. Required for NAT traversal
    /// against cellular peers. Adds ~negligible cost when no relay is in use.
    pub relay_enabled: bool,
    /// Enable dcutr for direct hole-punching once relayed. No effect if
    /// `relay_enabled` is false.
    pub dcutr_enabled: bool,
    /// Enable `GossipSub`. Off by default for internet — pub/sub over the
    /// open internet without rate limiting is a risk; enable per-session.
    pub gossipsub_enabled: bool,
}

impl Default for InternetConfig {
    fn default() -> Self {
        Self {
            protocol: "/thespan/internet/1.0.0".into(),
            quic_port: 0,
            tcp_port: 0,
            dial_addr: None,
            relay_reservation_addrs: Vec::new(),
            relay_enabled: true,
            dcutr_enabled: true,
            gossipsub_enabled: false,
        }
    }
}

/// Build a swarm with the default Thespan wire prefix.
pub fn build_internet_swarm_with_keypair(
    config: &InternetConfig,
    keypair: libp2p::identity::Keypair,
) -> Result<Swarm<ThespanBehaviour>, TransportError> {
    build_internet_swarm_with_config(config, keypair, &ThespanConfig::default())
}

/// Build a swarm with the application's Thespan wire prefix.
pub fn build_internet_swarm_with_config(
    config: &InternetConfig,
    keypair: libp2p::identity::Keypair,
    thespan_config: &ThespanConfig,
) -> Result<Swarm<ThespanBehaviour>, TransportError> {
    let gossipsub_enabled = config.gossipsub_enabled;
    let relay_enabled = config.relay_enabled;
    let dcutr_enabled = config.dcutr_enabled && relay_enabled;

    let swarm = SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            // libp2p::tcp::Config exposes
            // nodelay/ttl/port-reuse knobs in this version, but not a portable
            // SO_KEEPALIVE setter. LAN is QUIC-only; TCP remains an
            // internet fallback while ping + KeepAliveBehaviour carry liveness.
            libp2p::tcp::Config::default().nodelay(true),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .map_err(|e| TransportError::Transport(format!("tcp: {e}")))?
        .with_quic_config(|mut cfg| {
            // Allow longer idle periods for interactive applications.
            cfg.max_idle_timeout = INTERNET_QUIC_IDLE_TIMEOUT_MS;
            cfg
        })
        .with_relay_client(libp2p::noise::Config::new, libp2p::yamux::Config::default)
        .map_err(|e| TransportError::Transport(format!("relay client: {e}")))?
        .with_behaviour(|key, relay_client| {
            let local_peer_id = key.public().to_peer_id();

            // Relay client is provided by SwarmBuilder when `with_relay_client`
            // was called; we just decide whether to enable it via Toggle.
            let relay_toggle = if relay_enabled {
                Toggle::from(Some(relay_client))
            } else {
                Toggle::from(None)
            };

            let identify_toggle = if relay_enabled {
                Toggle::from(Some(libp2p::identify::Behaviour::new(
                    libp2p::identify::Config::new(
                        crate::session::applied_protocol("/thespan/internet/identify/1"),
                        key.public(),
                    ),
                )))
            } else {
                Toggle::from(None)
            };

            let dcutr_toggle = if dcutr_enabled {
                Toggle::from(Some(libp2p::dcutr::Behaviour::new(local_peer_id)))
            } else {
                Toggle::from(None)
            };

            build_behaviour(
                key,
                #[cfg(feature = "legacy-raw-codec")]
                &config.protocol,
                thespan_config,
                gossipsub_enabled,
                libp2p::ping::Config::new()
                    .with_interval(Duration::from_secs(10))
                    .with_timeout(Duration::from_secs(30)),
                relay_toggle,
                identify_toggle,
                dcutr_toggle,
            )
        })
        .map_err(|error| TransportError::Behaviour(error.to_string()))?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(SWARM_IDLE_CONNECTION_TIMEOUT))
        .build();

    Ok(swarm)
}

/// IPv4 QUIC listen multiaddr.
pub fn quic_listen_addr(config: &InternetConfig) -> Result<Multiaddr, TransportError> {
    quic_listen_addr_v4(config)
}

/// IPv4 QUIC listen multiaddr.
pub fn quic_listen_addr_v4(config: &InternetConfig) -> Result<Multiaddr, TransportError> {
    format!("/ip4/0.0.0.0/udp/{}/quic-v1", config.quic_port)
        .parse()
        .map_err(|e| TransportError::Transport(format!("invalid IPv4 QUIC listen multiaddr: {e}")))
}

/// IPv6 QUIC listen multiaddr.
pub fn quic_listen_addr_v6(config: &InternetConfig) -> Result<Multiaddr, TransportError> {
    format!("/ip6/::/udp/{}/quic-v1", config.quic_port)
        .parse()
        .map_err(|e| TransportError::Transport(format!("invalid IPv6 QUIC listen multiaddr: {e}")))
}

/// IPv4 TCP listen multiaddr.
pub fn tcp_listen_addr_v4(config: &InternetConfig) -> Result<Multiaddr, TransportError> {
    format!("/ip4/0.0.0.0/tcp/{}", config.tcp_port)
        .parse()
        .map_err(|e| TransportError::Transport(format!("invalid IPv4 TCP listen multiaddr: {e}")))
}

/// IPv6 TCP listen multiaddr.
pub fn tcp_listen_addr_v6(config: &InternetConfig) -> Result<Multiaddr, TransportError> {
    format!("/ip6/::/tcp/{}", config.tcp_port)
        .parse()
        .map_err(|e| TransportError::Transport(format!("invalid IPv6 TCP listen multiaddr: {e}")))
}

/// Additional listen addresses started after the primary IPv4 QUIC listener.
pub fn additional_listen_addrs(config: &InternetConfig) -> Result<Vec<Multiaddr>, TransportError> {
    let mut addrs = vec![
        quic_listen_addr_v6(config)?,
        tcp_listen_addr_v4(config)?,
        tcp_listen_addr_v6(config)?,
    ];

    if config.relay_enabled {
        addrs.extend(
            config
                .relay_reservation_addrs
                .iter()
                .filter_map(relay_reservation_listen_addr),
        );
    }

    Ok(addrs)
}

/// Relay server peer IDs reserved by this internet transport instance.
pub fn infrastructure_peer_ids(config: &InternetConfig) -> HashSet<PeerId> {
    if !config.relay_enabled {
        return HashSet::new();
    }

    config
        .relay_reservation_addrs
        .iter()
        .filter_map(relay_peer_id)
        .collect()
}

fn relay_peer_id(relay_addr: &Multiaddr) -> Option<PeerId> {
    match relay_addr.iter().last() {
        Some(Protocol::P2p(peer_id)) => Some(peer_id),
        _ => None,
    }
}

/// Convert a relay peer address into the listen address that requests a Relay v2
/// reservation from that peer.
pub fn relay_reservation_listen_addr(relay_addr: &Multiaddr) -> Option<Multiaddr> {
    if !relay_addr
        .iter()
        .any(|protocol| matches!(protocol, Protocol::P2p(_)))
    {
        return None;
    }

    if relay_addr
        .iter()
        .any(|protocol| matches!(protocol, Protocol::P2pCircuit))
    {
        return Some(relay_addr.clone());
    }

    let mut addr = relay_addr.clone();
    addr.push(Protocol::P2pCircuit);
    Some(addr)
}
