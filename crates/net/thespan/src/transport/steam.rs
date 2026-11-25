//! Steam transport: ISteamNetworkingSockets + Yamux.
//!
//! Builds a libp2p Swarm using `SteamTransport` from the `libp2p-steam-transport`
//! crate with Yamux multiplexing over the raw reliable Steam connection.
//! No Noise: Steam Datagram Relay provides encryption.
//! Peer discovery happens via Steam lobby.

use std::sync::Arc;
use std::time::Duration;

use libp2p::SwarmBuilder;
use libp2p::Transport as _;
use libp2p::core::ConnectedPoint;
use libp2p::core::muxing::StreamMuxerBox;
use libp2p::core::upgrade::{InboundConnectionUpgrade, OutboundConnectionUpgrade};
use libp2p::swarm::Swarm;
use libp2p::swarm::behaviour::toggle::Toggle;

use crate::actor::behavior::{ThespanBehaviour, build_behaviour};
use crate::config::ThespanConfig;

#[derive(Debug, thiserror::Error)]
pub enum SteamSwarmError {
    #[error("transport setup failed: {0}")]
    Transport(String),
    #[error("behaviour setup failed: {0}")]
    Behaviour(String),
}

/// Build a libp2p Swarm configured for Steam P2P with a pre-generated keypair.
///
/// Transport stack: `SteamTransport → PeerId handshake → Yamux`.
/// No Noise: Steam provides encryption via SDR.
pub fn build_steam_swarm_with_keypair(
    networking_sockets: libp2p_steam_transport::NetworkingSockets,
    virtual_port: i32,
    local_steam_id: u64,
    keypair: libp2p::identity::Keypair,
    dial_virtual_port: Option<i32>,
    thespan_config: &ThespanConfig,
) -> Result<Swarm<ThespanBehaviour>, SteamSwarmError> {
    let local_peer_id = keypair.public().to_peer_id();

    let sockets = Arc::new(networking_sockets);

    let swarm = SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_other_transport(move |_keypair| {
            let steam_transport = libp2p_steam_transport::SteamTransport::new(
                libp2p_steam_transport::SteamTransportConfig {
                    networking_sockets: sockets.clone(),
                    virtual_port,
                    local_steam_id,
                    dial_virtual_port,
                },
            );

            // Use and_then to perform async PeerId handshake + muxer setup
            // after the raw SteamStream connection is established.
            Ok(steam_transport
                .and_then(move |stream, connected_point| {
                    async move {
                        // 1. PeerId handshake (exchange public keys, no encryption needed)
                        let remote_peer_id =
                            libp2p_steam_transport::peer_id_handshake(&stream, &local_peer_id)
                                .await
                                .map_err(|e| {
                                    libp2p_steam_transport::SteamMuxerError::Steam(format!(
                                        "handshake failed: {e}"
                                    ))
                                })?;

                        // 2. Multiplex libp2p substreams over the reliable Steam stream.
                        let is_dialer = matches!(connected_point, ConnectedPoint::Dialer { .. });
                        let yamux_config = libp2p::yamux::Config::default();
                        let muxer = if is_dialer {
                            yamux_config
                                .upgrade_outbound(stream, "/yamux/1.0.0")
                                .await
                                .map_err(libp2p_steam_transport::SteamMuxerError::Io)?
                        } else {
                            yamux_config
                                .upgrade_inbound(stream, "/yamux/1.0.0")
                                .await
                                .map_err(libp2p_steam_transport::SteamMuxerError::Io)?
                        };

                        Ok::<_, libp2p_steam_transport::SteamMuxerError>((
                            remote_peer_id,
                            StreamMuxerBox::new(muxer),
                        ))
                    }
                })
                .boxed())
        })
        .map_err(|e| SteamSwarmError::Transport(e.to_string()))?
        .with_behaviour(|key: &libp2p::identity::Keypair| {
            build_behaviour(
                key,
                #[cfg(feature = "legacy-raw-codec")]
                &thespan_config.steam_protocol(),
                thespan_config,
                false,
                libp2p::ping::Config::new()
                    .with_interval(Duration::from_secs(15))
                    .with_timeout(Duration::from_secs(10)),
                Toggle::from(None),
                Toggle::from(None),
                Toggle::from(None),
            )
        })
        .map_err(|e| SteamSwarmError::Behaviour(e.to_string()))?
        .with_swarm_config(|cfg: libp2p::swarm::Config| {
            cfg.with_idle_connection_timeout(Duration::from_secs(300))
        })
        .build();

    Ok(swarm)
}

/// Build the listen multiaddr for a Steam transport.
pub fn listen_addr(local_steam_id: u64) -> libp2p::Multiaddr {
    libp2p_steam_transport::steam_multiaddr(local_steam_id)
}

/// Build the dial multiaddr for a remote Steam peer.
pub fn dial_addr(remote_steam_id: u64) -> libp2p::Multiaddr {
    libp2p_steam_transport::steam_multiaddr(remote_steam_id)
}
