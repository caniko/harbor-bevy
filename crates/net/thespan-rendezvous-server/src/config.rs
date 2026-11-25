use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Parser)]
#[command(name = "thespan-rendezvous", about = "Edge rendezvous + relay server")]
pub struct Config {
    /// QUIC listen address.
    #[arg(
        long,
        env = "THESPAN_RDV_QUIC",
        default_value = "/ip4/0.0.0.0/udp/4001/quic-v1"
    )]
    pub listen_quic: String,

    /// TCP listen address (Noise + Yamux).
    #[arg(long, env = "THESPAN_RDV_TCP", default_value = "/ip4/0.0.0.0/tcp/4001")]
    pub listen_tcp: String,

    /// IPv6 QUIC listen address.
    #[arg(
        long,
        env = "THESPAN_RDV_QUIC_V6",
        default_value = "/ip6/::/udp/4001/quic-v1"
    )]
    pub listen_quic_v6: String,

    /// IPv6 TCP listen address (Noise + Yamux).
    #[arg(long, env = "THESPAN_RDV_TCP_V6", default_value = "/ip6/::/tcp/4001")]
    pub listen_tcp_v6: String,

    /// Disable IPv6 listeners for v4-only emergency deploys.
    #[arg(long, env = "THESPAN_RDV_DISABLE_IPV6", default_value_t = false)]
    pub disable_ipv6: bool,

    /// External (advertised) address. Optional; for use behind NAT/load balancers.
    #[arg(long, env = "THESPAN_RDV_EXT")]
    pub external_addr: Option<String>,

    /// Path to ed25519 keypair file (libp2p-identity protobuf encoding).
    /// Generated and persisted on first start if absent.
    #[arg(
        long,
        env = "THESPAN_RDV_KEY",
        default_value = "/var/lib/thespan-rendezvous/identity.key"
    )]
    pub key_path: PathBuf,

    /// Maximum live registrations (across all namespaces).
    /// Sized to fit comfortably in <100MB RSS.
    #[arg(long, env = "THESPAN_RDV_MAX_REGS", default_value_t = 50_000)]
    pub max_registrations: usize,

    /// Per-source-IP request rate (per second).
    #[arg(long, env = "THESPAN_RDV_RATE", default_value_t = 20)]
    pub rate_limit_per_sec: u32,

    /// Accepted clock-skew tolerance for `SignedEnvelope.issued_at`, seconds.
    #[arg(long, env = "THESPAN_RDV_SKEW", default_value_t = 60)]
    pub clock_skew_secs: u64,

    /// Replay-cache retention window, seconds. Should be ≥ `clock_skew_secs`.
    #[arg(long, env = "THESPAN_RDV_REPLAY", default_value_t = 120)]
    pub replay_window_secs: u64,

    /// Emit JSON tracing records instead of compact text.
    #[arg(long, env = "THESPAN_RDV_LOG_JSON", default_value_t = false)]
    pub log_json: bool,

    /// Prometheus text metrics listener. Bind localhost unless explicitly fronted.
    #[arg(
        long,
        env = "THESPAN_RDV_METRICS_ADDR",
        default_value = "127.0.0.1:9617"
    )]
    pub metrics_addr: SocketAddr,

    /// Maximum live Circuit Relay v2 reservations.
    #[arg(
        long,
        env = "THESPAN_RDV_RELAY_MAX_RESERVATIONS",
        default_value_t = 1024
    )]
    pub relay_max_reservations: usize,

    /// Maximum live Circuit Relay v2 reservations per peer.
    #[arg(
        long,
        env = "THESPAN_RDV_RELAY_MAX_RESERVATIONS_PER_PEER",
        default_value_t = 4
    )]
    pub relay_max_reservations_per_peer: usize,

    /// Circuit Relay v2 reservation lifetime, seconds. Clients renew before expiry.
    #[arg(
        long,
        env = "THESPAN_RDV_RELAY_RESERVATION_SECS",
        default_value_t = 7200
    )]
    pub relay_reservation_secs: u64,

    /// Maximum concurrent relay circuits.
    #[arg(long, env = "THESPAN_RDV_RELAY_MAX_CIRCUITS", default_value_t = 256)]
    pub relay_max_circuits: usize,

    /// Maximum concurrent relay circuits per source peer.
    #[arg(
        long,
        env = "THESPAN_RDV_RELAY_MAX_CIRCUITS_PER_PEER",
        default_value_t = 8
    )]
    pub relay_max_circuits_per_peer: usize,

    /// Maximum relay circuit lifetime, seconds.
    #[arg(long, env = "THESPAN_RDV_RELAY_CIRCUIT_SECS", default_value_t = 1800)]
    pub relay_max_circuit_secs: u64,

    /// Maximum bytes per relay circuit.
    #[arg(
        long,
        env = "THESPAN_RDV_RELAY_CIRCUIT_BYTES",
        default_value_t = 67_108_864
    )]
    pub relay_max_circuit_bytes: u64,
}

impl Config {
    pub fn clock_skew(&self) -> Duration {
        Duration::from_secs(self.clock_skew_secs)
    }
    pub fn replay_window(&self) -> Duration {
        Duration::from_secs(self.replay_window_secs)
    }

    pub fn relay_config(&self) -> libp2p::relay::Config {
        libp2p::relay::Config {
            max_reservations: self.relay_max_reservations,
            max_reservations_per_peer: self.relay_max_reservations_per_peer,
            reservation_duration: Duration::from_secs(self.relay_reservation_secs),
            max_circuits: self.relay_max_circuits,
            max_circuits_per_peer: self.relay_max_circuits_per_peer,
            max_circuit_duration: Duration::from_secs(self.relay_max_circuit_secs),
            max_circuit_bytes: self.relay_max_circuit_bytes,
            ..Default::default()
        }
    }
}
