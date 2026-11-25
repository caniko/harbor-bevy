//! Session isolation: re-export of [`piying::remote::session::applied_protocol`].
//!
//! Behaviour:
//!
//! - With the `session-isolation` Cargo feature off (production default), the
//!   helper is a pass-through — it returns the base id unchanged. Callers
//!   that bake the result into a `LazyLock<StreamProtocol>` see no extra
//!   work versus a `const StreamProtocol::new`.
//! - With the feature on and `THESPAN_SESSION_ID` set in the environment at
//!   process start, every libp2p protocol id and gossipsub topic that flows
//!   through this helper is suffixed with `/{id}`. Two sessions that pick
//!   different ids cannot complete libp2p protocol negotiation against each
//!   other — even if they cohabit the same UDP port.
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
