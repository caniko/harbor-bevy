//! WebRTC Direct transport: UDP + DTLS + SCTP, exposed through libp2p.

use std::time::Duration;

use libp2p::swarm::Swarm;
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::{Multiaddr, SwarmBuilder, Transport};
use libp2p_webrtc::tokio::Certificate;

use crate::actor::behavior::{ThespanBehaviour, build_behaviour};
use crate::config::{ThespanConfig, WebRtcConfig};
use crate::transport::{SWARM_IDLE_CONNECTION_TIMEOUT, TransportError};

/// Build a libp2p Swarm configured for WebRTC Direct.
///
/// This keeps the same behaviours as LAN, including piying remote actor
/// messaging, but swaps the carrier transport to `/webrtc-direct`.
pub fn build_webrtc_swarm_with_keypair(
    config: &WebRtcConfig,
    keypair: libp2p::identity::Keypair,
    thespan_config: &ThespanConfig,
) -> Result<Swarm<ThespanBehaviour>, TransportError> {
    // WebRTC's DTLS verifier uses the process default. Cargo can unify ring
    // and AWS-LC through other clients, which disables Rustls's auto-selection.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let gossipsub_enabled = config.gossipsub_enabled;
    let transport_keypair = keypair.clone();
    // Reuse the persisted certificate when the caller supplies one: the
    // listener's `/certhash/...` is the fingerprint of this certificate, and a
    // peer that stored our address can only redial the same hash.
    let certificate = listener_certificate(config.certificate_pem.as_deref())?;

    let swarm = SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_other_transport(move |_keypair| {
            Ok(
                libp2p_webrtc::tokio::Transport::new(
                    transport_keypair.clone(),
                    certificate.clone(),
                )
                .boxed(),
            )
        })
        .map_err(|e| TransportError::Transport(e.to_string()))?
        .with_behaviour(|key| {
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

/// Generate a listener certificate and return it as PEM.
///
/// Callers persist the returned PEM and hand it back through
/// [`WebRtcConfig::certificate_pem`] so a restarted listener keeps the same
/// `/certhash/...` and its stored dial addresses stay usable.
pub fn generate_certificate_pem() -> Result<String, TransportError> {
    Ok(generate_certificate()?.serialize_pem())
}

/// The listener certificate for a spawn: the persisted PEM, or a fresh one.
///
/// An unreadable persisted PEM is replaced instead of failing the whole
/// transport: hosting must still work, it just loses the stable address.
fn listener_certificate(pem: Option<&str>) -> Result<Certificate, TransportError> {
    match pem {
        Some(pem) => Certificate::from_pem(pem).or_else(|error| {
            tracing::warn!(
                target: "thespan::transport",
                %error,
                "Persisted WebRTC certificate is unusable; minting a fresh one"
            );
            generate_certificate()
        }),
        None => generate_certificate(),
    }
}

fn generate_certificate() -> Result<Certificate, TransportError> {
    Certificate::generate(&mut rand08::thread_rng())
        .map_err(|error| TransportError::Transport(error.to_string()))
}

/// Build the listen multiaddr for WebRTC Direct.
pub fn listen_addr(config: &WebRtcConfig) -> Result<Multiaddr, TransportError> {
    format!("/ip4/0.0.0.0/udp/{}/webrtc-direct", config.listen_port)
        .parse()
        .map_err(|e| TransportError::Transport(format!("invalid WebRTC listen multiaddr: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listen_addr_uses_webrtc_direct() {
        let config = WebRtcConfig {
            listen_port: 27200,
            ..Default::default()
        };

        assert_eq!(
            listen_addr(&config)
                .expect("listen addr parses")
                .to_string(),
            "/ip4/0.0.0.0/udp/27200/webrtc-direct"
        );
    }

    #[test]
    fn a_persisted_certificate_is_reused_instead_of_regenerated() {
        // The defect: every spawn minted a new certificate, so the
        // `/certhash/...` a survivor stored named a certificate that no longer
        // existed after a restart.
        let pem = generate_certificate_pem().expect("generate certificate");
        let persisted = listener_certificate(Some(&pem)).expect("persisted certificate");
        let same_certificate = Certificate::from_pem(&pem).expect("parse persisted certificate");
        assert_eq!(persisted.fingerprint(), same_certificate.fingerprint());

        let fresh = listener_certificate(None).expect("generated certificate");
        assert_ne!(fresh.fingerprint(), persisted.fingerprint());
    }

    #[test]
    fn an_unreadable_persisted_certificate_does_not_block_hosting() {
        let certificate = listener_certificate(Some("not a certificate")).expect("fallback");
        // Still a usable certificate, just not the stale one.
        assert_ne!(certificate.serialize_pem(), "");
    }
}
