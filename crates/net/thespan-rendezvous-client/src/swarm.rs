//! Client-side `NetworkBehaviour` composition.
//!
//! Beyond the rendezvous request-response we also enable:
//! - `relay::client` + `dcutr` so we can dial peers behind NATs through the
//!   server's relay hop and then upgrade to direct via hole-punching.
//! - `identify` + `ping` (operational hygiene; identify also publishes our
//!   observed addresses via `AutoNAT`).

// `NetworkBehaviour`'s derive emits an unreachable `return` in its generated
// delegate dispatch, and an item-level allow does not reach derive output, so
// scope the allow to this module rather than the crate.
#![allow(unreachable_code)]

use crate::codec::FrameCodec;
use libp2p::{
    StreamProtocol, dcutr, identify, identity, ping, relay, request_response,
    swarm::NetworkBehaviour,
};

// The `NetworkBehaviour` derive emits an unreachable `return` in its generated
// delegate dispatch; the module-level allow above covers it.
#[derive(NetworkBehaviour)]
pub struct ClientBehaviour {
    pub rendezvous: request_response::Behaviour<FrameCodec>,
    pub relay_client: relay::client::Behaviour,
    pub dcutr: dcutr::Behaviour,
    pub identify: identify::Behaviour,
    pub ping: ping::Behaviour,
}

impl ClientBehaviour {
    pub fn new(local_key: &identity::Keypair, relay_client: relay::client::Behaviour) -> Self {
        let local_peer_id = local_key.public().to_peer_id();
        let rendezvous = request_response::Behaviour::with_codec(
            FrameCodec,
            std::iter::once((
                StreamProtocol::new(thespan_rendezvous_proto::PROTOCOL_ID),
                request_response::ProtocolSupport::Outbound,
            )),
            request_response::Config::default(),
        );
        let dcutr = dcutr::Behaviour::new(local_peer_id);
        let identify = identify::Behaviour::new(identify::Config::new(
            "/thespan/rendezvous/1".into(),
            local_key.public(),
        ));
        let ping = ping::Behaviour::new(ping::Config::default());
        Self {
            rendezvous,
            relay_client,
            dcutr,
            identify,
            ping,
        }
    }
}
