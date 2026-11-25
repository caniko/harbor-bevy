//! `SteamLaneMuxer` integration tests (Steam P2P loopback).
//!
//! Requires Steam client + App 480. All `#[ignore]`.
//!
//! ```sh
//! cargo test -p libp2p-steam-transport --test muxer_loopback -- --ignored
//! ```

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::io::{AsyncReadExt, AsyncWriteExt};
use libp2p_core::muxing::StreamMuxer;
use libp2p_steam_transport::{SteamLaneMuxer, SteamStream, peer_id_handshake};

// =============================================================================
// Test context
// =============================================================================

/// Steam SDK context with background callback pump.
struct SteamTestCtx {
    client: steamworks::Client,
    stop: Arc<AtomicBool>,
    _pump: Option<std::thread::JoinHandle<()>>,
}

impl SteamTestCtx {
    fn try_new() -> Option<Self> {
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
                log::warn!(
                    "SKIPPING: Steam init failed ({e}). Is Steam running with a compatible SDK?"
                );
                None
            }
        }
    }

    fn sockets(&self) -> steamworks::networking_sockets::NetworkingSockets {
        self.client.networking_sockets()
    }

    fn utils(&self) -> steamworks::networking_utils::NetworkingUtils {
        self.client.networking_utils()
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

/// P2P loopback connection (self-connect) yielding both sides as `SteamStream`.
struct LoopbackPair {
    listener_stream: SteamStream,
    dialer_stream: SteamStream,
}

fn establish_loopback(ctx: &SteamTestCtx, listen_port: i32) -> LoopbackPair {
    let sockets = ctx.sockets();
    let steam_id = ctx.steam_id();

    // Create listen socket
    let listen_socket = sockets
        .create_listen_socket_p2p(listen_port, [])
        .expect("create_listen_socket_p2p failed");

    // Connect to self
    let identity = steamworks::networking_types::NetworkingIdentity::new_steam_id(
        steamworks::SteamId::from_raw(steam_id),
    );
    let dial_conn = sockets
        .connect_p2p(identity, listen_port, [])
        .expect("connect_p2p failed");

    // Poll until both sides connect
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut dial_connected = false;
    let mut accepted_conn: Option<steamworks::networking_sockets::NetConnection> = None;

    while std::time::Instant::now() < deadline {
        // Check dialer state
        if !dial_connected
            && let Ok(info) = sockets.get_connection_info(&dial_conn)
            && info
                .state()
                .is_ok_and(|state| format!("{state:?}") == "Connected")
        {
            dial_connected = true;
        }

        // Accept on listener
        if accepted_conn.is_none() {
            while let Some(event) = listen_socket.try_receive_event() {
                match event {
                    steamworks::networking_types::ListenSocketEvent::Connecting(req) => {
                        let _ = req.accept();
                    }
                    steamworks::networking_types::ListenSocketEvent::Connected(conn) => {
                        accepted_conn = Some(conn.take_connection());
                    }
                    steamworks::networking_types::ListenSocketEvent::Disconnected(_) => {}
                }
            }
        }

        if dial_connected && accepted_conn.is_some() {
            break;
        }

        std::thread::sleep(Duration::from_millis(16));
    }

    assert!(dial_connected, "dialer should connect within 5s");
    let listen_conn = accepted_conn.expect("listener should accept within 5s");

    LoopbackPair {
        listener_stream: SteamStream::new(listen_conn),
        dialer_stream: SteamStream::new(dial_conn),
    }
}

// =============================================================================
// PeerId handshake
// =============================================================================

/// `PeerId` exchange over Steam loopback.
#[test]
#[ignore]
fn peer_id_handshake_loopback() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let pair = establish_loopback(&ctx, 200);

        let key_a = libp2p_identity::Keypair::ed25519_from_bytes(&mut [1u8; 32]).unwrap();
        let key_b = libp2p_identity::Keypair::ed25519_from_bytes(&mut [2u8; 32]).unwrap();
        let peer_a = key_a.public().to_peer_id();
        let peer_b = key_b.public().to_peer_id();

        // Run handshake concurrently on both sides
        let (remote_of_a, remote_of_b) = tokio::join!(
            peer_id_handshake(&pair.dialer_stream, &peer_a),
            peer_id_handshake(&pair.listener_stream, &peer_b),
        );

        let remote_of_a = remote_of_a.expect("dialer handshake should succeed");
        let remote_of_b = remote_of_b.expect("listener handshake should succeed");

        assert_eq!(remote_of_a, peer_b, "dialer should see listener's PeerId");
        assert_eq!(remote_of_b, peer_a, "listener should see dialer's PeerId");
    });
}

// =============================================================================
// Muxer substream tests
// =============================================================================

/// Create a muxer pair: A=initiator (even lanes), B=responder (odd lanes).
fn make_muxer_pair(
    ctx: &SteamTestCtx,
    listen_port: i32,
    initial_lanes: i32,
) -> (SteamLaneMuxer, SteamLaneMuxer) {
    let pair = establish_loopback(ctx, listen_port);

    let sockets_a = Arc::new(ctx.sockets());
    let utils_a = Arc::new(ctx.utils());
    let sockets_b = Arc::new(ctx.sockets());
    let utils_b = Arc::new(ctx.utils());

    let muxer_a =
        SteamLaneMuxer::from_stream(pair.dialer_stream, sockets_a, utils_a, initial_lanes, true)
            .expect("muxer A creation should succeed");
    let muxer_b = SteamLaneMuxer::from_stream(
        pair.listener_stream,
        sockets_b,
        utils_b,
        initial_lanes,
        false,
    )
    .expect("muxer B creation should succeed");

    (muxer_a, muxer_b)
}

/// Drive the muxer's `poll()` for `ticks` cycles.
async fn drive_muxer(muxer: &mut SteamLaneMuxer, ticks: usize) {
    for _ in 0..ticks {
        let _ = futures::future::poll_fn(|cx| {
            let _ = Pin::new(&mut *muxer).poll(cx);
            std::task::Poll::Ready(())
        })
        .await;
        tokio::time::sleep(Duration::from_millis(16)).await;
    }
}

/// Non-blocking `poll()` + `poll_inbound()` loop. Panics on timeout.
async fn drive_until_inbound(
    muxer: &mut SteamLaneMuxer,
    timeout: Duration,
) -> Result<<SteamLaneMuxer as StreamMuxer>::Substream, <SteamLaneMuxer as StreamMuxer>::Error> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        // Drive poll() then try poll_inbound() — never block on Pending
        let maybe = futures::future::poll_fn(|cx| {
            let _ = Pin::new(&mut *muxer).poll(cx);
            match Pin::new(&mut *muxer).poll_inbound(cx) {
                std::task::Poll::Ready(result) => std::task::Poll::Ready(Some(result)),
                std::task::Poll::Pending => std::task::Poll::Ready(None),
            }
        })
        .await;

        if let Some(result) = maybe {
            return result;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for inbound substream"
        );
        tokio::time::sleep(Duration::from_millis(16)).await;
    }
}

/// A opens substream, B accepts, A writes, B reads.
#[path = "muxer_loopback/basic.rs"]
mod basic;
#[path = "muxer_loopback/edge.rs"]
mod edge;
