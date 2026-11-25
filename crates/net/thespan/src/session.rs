//! Session isolation: re-export of [`piying::remote::session::applied_protocol`].
//!
//! Behaviour:
//!
//! - With the `session-isolation` Cargo feature off (production default), the
//!   helper is a pass-through. It is also a pass-through when the ID is unset
//!   or empty.
//! - With the feature on and a nonempty `THESPAN_SESSION_ID`, application wire
//!   IDs and GossipSub topics are suffixed with `/{id}`. Set it before the
//!   first use; piying caches it for the process lifetime.
//!
//! Standard transport and infrastructure protocols (e.g. ping, identify and
//! public relay) stay interoperable. Session scoping is not a connection or
//! authentication gate.
//!
//! Forwarded into `piying` so the registry/messaging protocol ids it owns
//! get the same suffix without thespan having to mirror the strings.

pub use piying::remote::session::applied_protocol;

/// Build a [`libp2p::StreamProtocol`] from a base id, applying session
/// isolation if active. Panics if the resulting id does not start with `/`.
pub fn stream_protocol(base: &str) -> libp2p::StreamProtocol {
    libp2p::StreamProtocol::try_from_owned(applied_protocol(base))
        .expect("protocol id must start with '/'")
}

/// Build a [`libp2p::gossipsub::IdentTopic`] from a base topic, applying
/// session isolation if active.
pub fn gossipsub_topic(base: &str) -> libp2p::gossipsub::IdentTopic {
    libp2p::gossipsub::IdentTopic::new(applied_protocol(base))
}

/// Restore the application topic name and reject an unexpected wire suffix.
pub(crate) fn base_gossipsub_topic(wire: &str) -> Option<&str> {
    static SUFFIX: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| applied_protocol(""));
    wire.strip_suffix(SUFFIX.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::core::upgrade::UpgradeInfo;
    use libp2p::swarm::{ConnectionHandler, ConnectionId, NetworkBehaviour};

    #[tokio::test]
    async fn session_wire_ids() {
        const CHILD: &str = "THESPAN_SESSION_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // The suffix is process-cached. Child processes avoid resetting it
            // or mutating the parent environment alongside other tests.
            for id in [None, Some(""), Some("session-42")] {
                let mut child = std::process::Command::new(std::env::current_exe().unwrap());
                child.args(["--exact", "session::tests::session_wire_ids", "--nocapture"]);
                child.env(CHILD, "1");
                match id {
                    Some(id) => child.env("THESPAN_SESSION_ID", id),
                    None => child.env_remove("THESPAN_SESSION_ID"),
                };
                let output = child.output().unwrap();
                assert!(
                    output.status.success(),
                    "id={id:?}: {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                );
            }
            return;
        }

        let id = std::env::var("THESPAN_SESSION_ID").unwrap_or_default();
        let scoped = cfg!(feature = "session-isolation") && !id.is_empty();
        let expected = |base: &str| {
            if scoped {
                format!("{base}/{id}")
            } else {
                base.to_owned()
            }
        };

        let topic = "lobby/updates";
        let wire = gossipsub_topic(topic).hash();
        assert_eq!(wire.as_str(), expected(topic));
        assert_eq!(base_gossipsub_topic(wire.as_str()), Some(topic));
        assert_eq!(base_gossipsub_topic(topic), (!scoped).then_some(topic));

        let config = crate::ThespanConfig::default();
        let key = libp2p::identity::Keypair::generate_ed25519();
        let mut behaviour = crate::actor::behavior::build_behaviour(
            &key,
            #[cfg(feature = "legacy-raw-codec")]
            &config.steam_protocol(),
            &config,
            true,
            libp2p::ping::Config::default(),
            None.into(),
            None.into(),
            None.into(),
        )
        .unwrap();
        let addr = "/memory/0".parse().unwrap();
        let handler = behaviour
            .handle_established_inbound_connection(
                ConnectionId::new_unchecked(1),
                libp2p::identity::Keypair::generate_ed25519()
                    .public()
                    .to_peer_id(),
                &addr,
                &addr,
            )
            .unwrap();
        let listen = handler.listen_protocol();
        let ids: Vec<_> = listen
            .upgrade()
            .protocol_info()
            .map(|id| AsRef::<str>::as_ref(&id).to_owned())
            .collect();
        for base in [
            config.keepalive_protocol().as_str(),
            "/piying/registry/1.0.0",
            "/piying/messaging/1.0.0",
            "/meshsub/1.1.0",
            #[cfg(feature = "legacy-raw-codec")]
            config.steam_protocol().as_str(),
        ] {
            assert!(ids.contains(&expected(base)), "{base}: {ids:?}");
            if scoped {
                assert!(!ids.iter().any(|id| id == base), "{ids:?}");
            }
        }
        for base in ["/meshsub/1.0.0", "/meshsub/1.2.0"] {
            assert_eq!(ids.iter().any(|id| id == base), !scoped, "{ids:?}");
        }
        #[cfg(not(feature = "legacy-raw-codec"))]
        assert!(
            !ids.contains(&expected(&config.steam_protocol())),
            "{ids:?}"
        );
    }
}
