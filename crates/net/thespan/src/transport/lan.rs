//! LAN transport: QUIC (UDP, TLS 1.3, native stream multiplexing).

use std::time::Duration;

use libp2p::swarm::Swarm;
use libp2p::{Multiaddr, SwarmBuilder};

use libp2p::swarm::behaviour::toggle::Toggle;

use crate::actor::behavior::{ThespanBehaviour, build_behaviour};
use crate::config::{LanConfig, ThespanConfig};
use crate::transport::{SWARM_IDLE_CONNECTION_TIMEOUT, TransportError};

/// Build a libp2p Swarm for a LAN connection with a pre-generated keypair.
///
/// Transport stack: QUIC (UDP + TLS 1.3 + native stream multiplexing)
/// Discovery: direct dial or rendezvous-provided addresses.
///
/// The keypair is accepted as a parameter so the caller can extract the `PeerId`
/// synchronously before the swarm is built inside a tokio runtime context.
pub fn build_lan_swarm_with_keypair(
    config: &LanConfig,
    keypair: libp2p::identity::Keypair,
    thespan_config: &ThespanConfig,
) -> Result<Swarm<ThespanBehaviour>, TransportError> {
    let gossipsub_enabled = config.gossipsub_enabled;

    let swarm = SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_quic_config(|mut cfg| {
            // libp2p-quic defaults: max_idle_timeout = 10s, keep_alive_interval = 5s.
            // Allow longer idle periods for interactive applications.
            cfg.max_idle_timeout = 60_000;
            cfg
        })
        .with_behaviour(|key| {
            // Ping at 10s intervals — keeps connections alive via periodic traffic.
            // This supplements the KeepAlive substream approach, which can fail silently
            // if protocol negotiation doesn't complete (causing 25s disconnect).
            build_behaviour(
                key,
                #[cfg(feature = "legacy-raw-codec")]
                &config.protocol,
                thespan_config,
                gossipsub_enabled,
                libp2p::ping::Config::new()
                    .with_interval(Duration::from_secs(10))
                    .with_timeout(Duration::from_secs(30)),
                Toggle::from(None),
                Toggle::from(None),
                Toggle::from(None),
            )
        })
        .map_err(|error| TransportError::Behaviour(error.to_string()))?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(SWARM_IDLE_CONNECTION_TIMEOUT))
        .build();

    Ok(swarm)
}

/// Build the listen multiaddr for a LAN config (QUIC over UDP).
pub fn listen_addr(config: &LanConfig) -> Result<Multiaddr, TransportError> {
    format!("/ip4/0.0.0.0/udp/{}/quic-v1", config.listen_port)
        .parse()
        .map_err(|e| TransportError::Transport(format!("invalid LAN listen multiaddr: {e}")))
}
