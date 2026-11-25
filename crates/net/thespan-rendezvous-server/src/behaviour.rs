//! Composed `NetworkBehaviour` for the server.
//!
//! Behaviors:
//! - `request_response` over our [`FrameCodec`] — the rendezvous protocol itself.
//! - `relay::Behaviour` — Circuit Relay v2 hop, so NAT'd peers can be reached.
//! - `identify` + `ping` — operational hygiene.

// `NetworkBehaviour`'s derive emits an unreachable `return` in its generated
// delegate dispatch, and an item-level allow does not reach derive output, so
// scope the allow to this module rather than the crate.
#![allow(unreachable_code)]

use crate::codec::FrameCodec;
use libp2p::{
    StreamProtocol, identify, identity, ping, relay, request_response, swarm::NetworkBehaviour,
};

// The `NetworkBehaviour` derive emits an unreachable `return` in its generated
// delegate dispatch; the module-level allow above covers it.
#[derive(NetworkBehaviour)]
pub struct Behaviour {
    pub rendezvous: request_response::Behaviour<FrameCodec>,
    pub relay: relay::Behaviour,
    pub identify: identify::Behaviour,
    pub ping: ping::Behaviour,
}

impl Behaviour {
    pub fn new(local_key: &identity::Keypair) -> Self {
        Self::new_with_relay_config(local_key, relay::Config::default())
    }

    pub fn new_with_relay_config(
        local_key: &identity::Keypair,
        relay_config: relay::Config,
    ) -> Self {
        let local_peer_id = local_key.public().to_peer_id();

        let rendezvous = request_response::Behaviour::with_codec(
            FrameCodec,
            std::iter::once((
                StreamProtocol::new(thespan_rendezvous_proto::PROTOCOL_ID),
                request_response::ProtocolSupport::Full,
            )),
            request_response::Config::default(),
        );

        let relay = relay::Behaviour::new(local_peer_id, relay_config);

        let identify = identify::Behaviour::new(identify::Config::new(
            "/thespan/rendezvous/1".into(),
            local_key.public(),
        ));

        let ping = ping::Behaviour::new(ping::Config::default());

        Self {
            rendezvous,
            relay,
            identify,
            ping,
        }
    }
}
