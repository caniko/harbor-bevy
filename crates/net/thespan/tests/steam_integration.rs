//! Integration tests for Steam P2P networking through thespan.
//!
//! Tests the full stack: `SteamTransport` + Yamux + piying `NetworkActor`.
//!
//! Requires:
//! - Steam client running and logged in
//! - App ID 480 (Valve Spacewar)
//! - The `steam` and `legacy-raw-codec` features enabled on thespan
//!
//! Run with:
//! ```sh
//! cargo test -p thespan --features steam,legacy-raw-codec --test steam_integration -- --ignored
//! ```

#![cfg(all(feature = "steam", feature = "legacy-raw-codec"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use thespan::actor::events::ActorEvent;
use thespan::config::{SteamConfig, TransportType};
use thespan::handle::NetworkHandle;

/// Test context that initializes Steam SDK and pumps callbacks.
struct SteamTestCtx {
    client: steamworks::Client,
    stop: Arc<AtomicBool>,
    _pump: Option<std::thread::JoinHandle<()>>,
}

impl SteamTestCtx {
    fn try_new() -> Option<Self> {
        let _ = env_logger::builder().is_test(true).try_init();

        match steamworks::Client::init_app(480u32) {
            Ok(client) => {
                let stop = Arc::new(AtomicBool::new(false));
                let client2 = client.clone();
                let stop2 = stop.clone();
                let pump = std::thread::spawn(move || {
                    while !stop2.load(Ordering::Relaxed) {
                        client2.run_callbacks();
                        std::thread::sleep(Duration::from_millis(16));
                    }
                });

                Some(Self {
                    client,
                    stop,
                    _pump: Some(pump),
                })
            }
            Err(e) => {
                tracing::warn!(
                    "SKIPPING: Steam init failed ({e}). Is Steam running with a compatible SDK?"
                );
                None
            }
        }
    }

    fn sockets(&self) -> steamworks::networking_sockets::NetworkingSockets {
        self.client.networking_sockets()
    }

    fn steam_id(&self) -> u64 {
        self.client.user().steam_id().raw()
    }
}

impl Drop for SteamTestCtx {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self._pump.take() {
            let _ = h.join();
        }
    }
}

/// Wait for a `PeerConnected` event on the given handle within `timeout`.
fn wait_for_peer_connected(handle: &NetworkHandle, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        while let Ok(event) = handle.event_rx.try_recv() {
            if matches!(event, ActorEvent::PeerConnected { .. }) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Drain events until we find a `MessageReceived` within `timeout`.
fn wait_for_message(handle: &NetworkHandle, timeout: Duration) -> Option<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        while let Ok(event) = handle.event_rx.try_recv() {
            if let ActorEvent::MessageReceived { payload, .. } = event {
                return Some(payload);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

// =============================================================================
// CONNECTION TESTS
// =============================================================================

/// Test that two thespan actors can connect via Steam P2P loopback.
///
/// Uses different virtual ports so the two actors don't connect to themselves:
/// - Actor A: listens on port 10, dials SteamID on port 11 (reaches B)
/// - Actor B: listens on port 11, dials SteamID on port 10 (reaches A)
#[test]
#[ignore]
fn steam_actors_connect_via_loopback() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };
    let steam_id = ctx.steam_id();

    // Actor A: listen=10, dial=11
    let handle_a = thespan::spawn_network_actor(TransportType::Steam(SteamConfig {
        networking_sockets: ctx.sockets(),
        virtual_port: 10,
        local_steam_id: steam_id,
        remote_steam_id: Some(steam_id),
        remote_virtual_port: Some(11),
    }))
    .expect("spawn Steam actor A");

    // Small delay to let A's listen socket initialize
    std::thread::sleep(Duration::from_millis(200));

    // Actor B: listen=11, dial=10
    let handle_b = thespan::spawn_network_actor(TransportType::Steam(SteamConfig {
        networking_sockets: ctx.sockets(),
        virtual_port: 11,
        local_steam_id: steam_id,
        remote_steam_id: Some(steam_id),
        remote_virtual_port: Some(10),
    }))
    .expect("spawn Steam actor B");

    // Wait for PeerConnected on both sides
    let a_connected = wait_for_peer_connected(&handle_a, Duration::from_secs(10));
    let b_connected = wait_for_peer_connected(&handle_b, Duration::from_secs(10));

    assert!(a_connected, "Actor A should see PeerConnected within 10s");
    assert!(b_connected, "Actor B should see PeerConnected within 10s");
}

/// Test that connected actors can exchange messages via request_response.
#[test]
#[ignore]
fn steam_actors_exchange_messages() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };
    let steam_id = ctx.steam_id();

    let handle_a = thespan::spawn_network_actor(TransportType::Steam(SteamConfig {
        networking_sockets: ctx.sockets(),
        virtual_port: 20,
        local_steam_id: steam_id,
        remote_steam_id: Some(steam_id),
        remote_virtual_port: Some(21),
    }))
    .expect("spawn Steam actor A");

    std::thread::sleep(Duration::from_millis(200));

    let handle_b = thespan::spawn_network_actor(TransportType::Steam(SteamConfig {
        networking_sockets: ctx.sockets(),
        virtual_port: 21,
        local_steam_id: steam_id,
        remote_steam_id: Some(steam_id),
        remote_virtual_port: Some(20),
    }))
    .expect("spawn Steam actor B");

    // Wait for connection
    assert!(
        wait_for_peer_connected(&handle_a, Duration::from_secs(10)),
        "A should connect"
    );
    let b_peer_id = {
        // Find B's PeerId from A's PeerConnected event — we already consumed it.
        // Use the local_peer_id from handle_b instead.
        handle_b.local_peer_id
    };

    // Also drain B's PeerConnected
    assert!(
        wait_for_peer_connected(&handle_b, Duration::from_secs(10)),
        "B should connect"
    );

    // A sends a message to B
    let payload = b"hello from A".to_vec();
    handle_a
        .send(thespan::actor::commands::NetworkCommand::SendMessage {
            payload: payload.clone(),
            target: thespan::actor::commands::SendTarget::Peer(b_peer_id),
        })
        .expect("queue message from A");

    // B should receive it
    let received = wait_for_message(&handle_b, Duration::from_secs(5));
    assert_eq!(
        received.as_deref(),
        Some(payload.as_slice()),
        "B should receive the message from A"
    );

    // B sends back to A
    let reply = b"hello from B".to_vec();
    handle_b
        .send(thespan::actor::commands::NetworkCommand::SendMessage {
            payload: reply.clone(),
            target: thespan::actor::commands::SendTarget::Peer(handle_a.local_peer_id),
        })
        .expect("queue reply from B");

    let received = wait_for_message(&handle_a, Duration::from_secs(5));
    assert_eq!(
        received.as_deref(),
        Some(reply.as_slice()),
        "A should receive the reply from B"
    );
}

/// Test graceful disconnect over Steam transport.
///
/// NOTE: Currently fails over loopback because `disconnect_peer_id` closes the
/// libp2p handler but the underlying Steam P2P socket stays alive (both peers
/// share the same process). B never sees `PeerDisconnected`. This is a known
/// limitation of Steam loopback testing — works correctly with two separate
/// processes/machines.
#[test]
#[ignore]
fn steam_graceful_disconnect() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };
    let steam_id = ctx.steam_id();

    let handle_a = thespan::spawn_network_actor(TransportType::Steam(SteamConfig {
        networking_sockets: ctx.sockets(),
        virtual_port: 30,
        local_steam_id: steam_id,
        remote_steam_id: Some(steam_id),
        remote_virtual_port: Some(31),
    }))
    .expect("spawn Steam actor A");

    std::thread::sleep(Duration::from_millis(200));

    let handle_b = thespan::spawn_network_actor(TransportType::Steam(SteamConfig {
        networking_sockets: ctx.sockets(),
        virtual_port: 31,
        local_steam_id: steam_id,
        remote_steam_id: Some(steam_id),
        remote_virtual_port: Some(30),
    }))
    .expect("spawn Steam actor B");

    // Wait for connection
    assert!(wait_for_peer_connected(&handle_a, Duration::from_secs(10)));
    assert!(wait_for_peer_connected(&handle_b, Duration::from_secs(10)));

    // A gracefully disconnects
    handle_a
        .send(thespan::actor::commands::NetworkCommand::GracefulDisconnect { peer_id: None })
        .expect("queue disconnect");

    // B should see PeerDisconnected (may also receive the BYE marker message first).
    // Steam connection teardown over loopback can be slow — use a generous timeout.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut b_saw_disconnect = false;
    while Instant::now() < deadline {
        while let Ok(event) = handle_b.event_rx.try_recv() {
            if matches!(event, ActorEvent::PeerDisconnected { .. }) {
                b_saw_disconnect = true;
            }
        }
        if b_saw_disconnect {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(
        b_saw_disconnect,
        "B should see PeerDisconnected after A's graceful disconnect"
    );
}

/// Send 10 messages in rapid succession A→B, verify all arrive.
/// Exercises the SteamLaneMuxer under realistic load (multiple substreams).
#[test]
#[ignore]
fn steam_actors_exchange_multiple_messages() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };
    let steam_id = ctx.steam_id();

    let handle_a = thespan::spawn_network_actor(TransportType::Steam(SteamConfig {
        networking_sockets: ctx.sockets(),
        virtual_port: 40,
        local_steam_id: steam_id,
        remote_steam_id: Some(steam_id),
        remote_virtual_port: Some(41),
    }))
    .expect("spawn Steam actor A");

    std::thread::sleep(Duration::from_millis(200));

    let handle_b = thespan::spawn_network_actor(TransportType::Steam(SteamConfig {
        networking_sockets: ctx.sockets(),
        virtual_port: 41,
        local_steam_id: steam_id,
        remote_steam_id: Some(steam_id),
        remote_virtual_port: Some(40),
    }))
    .expect("spawn Steam actor B");

    // Wait for connection
    assert!(
        wait_for_peer_connected(&handle_a, Duration::from_secs(10)),
        "A should connect"
    );
    assert!(
        wait_for_peer_connected(&handle_b, Duration::from_secs(10)),
        "B should connect"
    );

    let b_peer_id = handle_b.local_peer_id;

    // Send 10 messages rapidly from A to B
    let message_count = 10;
    let mut expected: Vec<Vec<u8>> = Vec::new();
    for i in 0..message_count {
        let payload = format!("rapid message #{i}").into_bytes();
        expected.push(payload.clone());
        handle_a
            .send(thespan::actor::commands::NetworkCommand::SendMessage {
                payload,
                target: thespan::actor::commands::SendTarget::Peer(b_peer_id),
            })
            .expect("queue message from A");
    }

    // Collect all 10 messages on B (with generous timeout)
    let mut received: Vec<Vec<u8>> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while received.len() < message_count && Instant::now() < deadline {
        while let Ok(event) = handle_b.event_rx.try_recv() {
            if let ActorEvent::MessageReceived { payload, .. } = event {
                received.push(payload);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert_eq!(
        received.len(),
        message_count,
        "B should receive all {message_count} messages (got {})",
        received.len()
    );

    // Verify all expected messages were received (order may vary due to
    // concurrent substreams, but each message should arrive exactly once)
    for exp in &expected {
        assert!(
            received.contains(exp),
            "missing message: {:?}",
            String::from_utf8_lossy(exp)
        );
    }
}
