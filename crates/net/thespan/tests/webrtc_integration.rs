//! Integration tests for WebRTC Direct networking through thespan.

use std::time::{Duration, Instant};

use thespan::actor::commands::{NetworkCommand, SendTarget};
use thespan::actor::events::ActorEvent;
use thespan::config::{TransportType, WebRtcConfig};
use thespan::handle::NetworkHandle;
use thespan::{Multiaddr, PeerId};

struct ActorGuard {
    handle: Option<NetworkHandle>,
}

impl ActorGuard {
    fn new(handle: NetworkHandle) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    fn handle(&self) -> &NetworkHandle {
        self.handle.as_ref().expect("actor handle present")
    }
}

impl Drop for ActorGuard {
    fn drop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };

        handle.send(NetworkCommand::Shutdown);
        handle.command.kill();
    }
}

fn wait_for_listen_addr(handle: &NetworkHandle, timeout: Duration) -> Multiaddr {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match handle.event_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(ActorEvent::ListenStarted { addr }) => return addr,
            Ok(ActorEvent::Error { error, .. }) => {
                panic!("network actor error before listen: {error}")
            }
            Ok(_) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                panic!("network actor event channel disconnected before listen")
            }
        }
    }

    panic!("timed out waiting for ListenStarted");
}

fn wait_for_peer_connected(handle: &NetworkHandle, timeout: Duration) -> PeerId {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match handle.event_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(ActorEvent::PeerConnected { peer_id, .. }) => return peer_id,
            Ok(ActorEvent::Error { error, .. }) => {
                panic!("network actor error before connection: {error}")
            }
            Ok(_) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                panic!("network actor event channel disconnected before connection")
            }
        }
    }

    panic!("timed out waiting for PeerConnected");
}

fn wait_for_message(handle: &NetworkHandle, timeout: Duration) -> Vec<u8> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match handle.event_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(ActorEvent::MessageReceived { payload, .. }) => return payload,
            Ok(ActorEvent::Error { error, .. }) => {
                panic!("network actor error before message: {error}")
            }
            Ok(_) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                panic!("network actor event channel disconnected before message")
            }
        }
    }

    panic!("timed out waiting for MessageReceived");
}

fn loopback_addr(listen_addr: Multiaddr) -> Multiaddr {
    let addr = listen_addr.to_string();
    addr.replacen("/ip4/0.0.0.0/", "/ip4/127.0.0.1/", 1)
        .parse()
        .expect("valid loopback WebRTC multiaddr")
}

#[test]
fn webrtc_direct_actors_connect_and_exchange_messages() {
    let _ = env_logger::builder().is_test(true).try_init();

    let peer_a = ActorGuard::new(
        thespan::spawn_network_actor_with_keypair(
            TransportType::WebRtc(WebRtcConfig {
                listen_port: 0,
                protocol: "/thespan/webrtc-e2e/1.0.0".into(),
                dial_addr: None,
                gossipsub_enabled: false,
                certificate_pem: None,
            }),
            thespan::identity::Keypair::generate_ed25519(),
            None,
        )
        .expect("spawn WebRTC peer A"),
    );

    let listen_addr = wait_for_listen_addr(peer_a.handle(), Duration::from_secs(10));
    let dial_addr = loopback_addr(listen_addr);

    let peer_b = ActorGuard::new(
        thespan::spawn_network_actor_with_keypair(
            TransportType::WebRtc(WebRtcConfig {
                listen_port: 0,
                protocol: "/thespan/webrtc-e2e/1.0.0".into(),
                dial_addr: Some(dial_addr),
                gossipsub_enabled: false,
                certificate_pem: None,
            }),
            thespan::identity::Keypair::generate_ed25519(),
            None,
        )
        .expect("spawn WebRTC peer B"),
    );

    let peer_b_id = wait_for_peer_connected(peer_a.handle(), Duration::from_secs(10));
    let peer_a_id = wait_for_peer_connected(peer_b.handle(), Duration::from_secs(10));

    assert_eq!(peer_a_id, peer_a.handle().local_peer_id);
    assert_eq!(peer_b_id, peer_b.handle().local_peer_id);

    let payload = b"hello over webrtc direct".to_vec();
    peer_a.handle().send(NetworkCommand::SendMessage {
        payload: payload.clone(),
        target: SendTarget::Peer(peer_b_id),
    });

    assert_eq!(
        wait_for_message(peer_b.handle(), Duration::from_secs(10)),
        payload
    );
}
