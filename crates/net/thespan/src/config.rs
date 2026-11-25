//! Application and transport configuration types.

/// Application identity and wire namespace shared by all Thespan transports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThespanConfig {
    /// Single directory name under the platform's data directory.
    pub app_id: String,
    /// Exact wire prefix, including the leading `/` and no trailing `/`.
    pub protocol_prefix: String,
    /// Environment variable overriding the directory that contains `identity.bin`.
    pub identity_dir_env: String,
}

impl ThespanConfig {
    pub fn new(
        app_id: impl Into<String>,
        protocol_prefix: impl Into<String>,
        identity_dir_env: impl Into<String>,
    ) -> Self {
        Self {
            app_id: app_id.into(),
            protocol_prefix: protocol_prefix.into(),
            identity_dir_env: identity_dir_env.into(),
        }
    }

    /// Base protocol ID negotiated by the keep-alive connection handler.
    pub fn keepalive_protocol(&self) -> String {
        format!("{}/keepalive/1.0.0", self.protocol_prefix)
    }

    /// Base legacy request-response protocol ID negotiated over Steam.
    pub fn steam_protocol(&self) -> String {
        format!("{}/steam/1.0.0", self.protocol_prefix)
    }
}

impl Default for ThespanConfig {
    fn default() -> Self {
        Self::new("thespan", "/thespan", crate::identity::IDENTITY_DIR_ENV)
    }
}

/// Selects which transport layer to use for networking.
pub enum TransportType {
    /// LAN: QUIC over UDP with direct dial or rendezvous discovery.
    Lan(LanConfig),
    /// WebRTC Direct over UDP. Dial addresses must include the listener's certhash.
    WebRtc(WebRtcConfig),
    /// Internet: TCP + QUIC, optionally with Circuit Relay v2 + dcutr
    /// for NAT traversal; discovery can flow through the rendezvous service.
    Internet(crate::transport::libp2p_internet::InternetConfig),
    /// Steam: P2P via ISteamNetworkingSockets and Yamux.
    #[cfg(feature = "steam")]
    Steam(SteamConfig),
}

/// Configuration for LAN (QUIC) transport.
#[derive(Debug, Clone)]
pub struct LanConfig {
    /// Port to listen on. Use 0 for OS-assigned.
    pub listen_port: u16,
    /// Legacy request-response protocol identifier (e.g. "/myapp/1.0.0").
    ///
    /// Must start with `/`. Ignored without `legacy-raw-codec`.
    pub protocol: String,
    /// Optional address to dial immediately on startup.
    /// Format: `/ip4/{ip}/udp/{port}/quic-v1`.
    pub dial_addr: Option<libp2p::Multiaddr>,
    /// Enable `GossipSub` for topic-based pub/sub.
    pub gossipsub_enabled: bool,
}

impl Default for LanConfig {
    fn default() -> Self {
        Self {
            listen_port: 0,
            protocol: "/thespan/1.0.0".into(),
            dial_addr: None,
            gossipsub_enabled: false,
        }
    }
}

/// Configuration for WebRTC Direct transport.
///
/// WebRTC Direct uses libp2p `/webrtc-direct` multiaddrs. A listening peer's
/// externally usable address includes a certificate hash appended by libp2p in
/// the `ListenStartedEvent`; joiners must dial that full address.
pub struct WebRtcConfig {
    /// Port to listen on. Use 0 for OS-assigned.
    pub listen_port: u16,
    /// Legacy request-response protocol identifier; ignored without `legacy-raw-codec`.
    pub protocol: String,
    /// Optional full `/webrtc-direct/certhash/...` address to dial on startup.
    pub dial_addr: Option<libp2p::Multiaddr>,
    /// Enable `GossipSub` for topic-based pub/sub.
    pub gossipsub_enabled: bool,
    /// Persisted listener certificate in PEM form, from
    /// [`crate::transport::webrtc::generate_certificate_pem`].
    ///
    /// Reusing it keeps the listener's `/certhash/...` stable across restarts,
    /// which is what makes a stored WebRTC address redialable. `None` mints a
    /// fresh certificate.
    pub certificate_pem: Option<String>,
}

impl Default for WebRtcConfig {
    fn default() -> Self {
        Self {
            listen_port: 0,
            protocol: "/thespan/1.0.0".into(),
            dial_addr: None,
            gossipsub_enabled: false,
            certificate_pem: None,
        }
    }
}

/// Configuration for Steam P2P transport.
///
/// Uses `ISteamNetworkingSockets` (connection-oriented) through the
/// `libp2p-steam-transport` crate, with Yamux over its reliable stream.
/// Steam provides transport encryption.
#[cfg(feature = "steam")]
pub struct SteamConfig {
    /// The `NetworkingSockets` handle from the Steam client.
    /// Obtained via `client.networking_sockets()`.
    pub networking_sockets: libp2p_steam_transport::NetworkingSockets,
    /// Virtual port for P2P listen sockets (typically 0).
    pub virtual_port: i32,
    /// The local user's SteamID64.
    pub local_steam_id: u64,
    /// The remote peer's SteamID64, or `None` for listen-only mode.
    pub remote_steam_id: Option<u64>,
    /// The remote peer's listen virtual port.
    /// If `None`, uses `virtual_port` for both listen and dial.
    /// Set this for loopback testing where both peers share the same SteamID.
    pub remote_virtual_port: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::ThespanConfig;

    #[test]
    fn protocol_prefix_preserves_wire_strings() {
        let config = ThespanConfig::new("example", "/example.v2", "EXAMPLE_IDENTITY_DIR");
        assert_eq!(config.keepalive_protocol(), "/example.v2/keepalive/1.0.0");
        assert_eq!(config.steam_protocol(), "/example.v2/steam/1.0.0");
        assert_eq!(
            ThespanConfig::default().keepalive_protocol(),
            "/thespan/keepalive/1.0.0"
        );
    }
}
