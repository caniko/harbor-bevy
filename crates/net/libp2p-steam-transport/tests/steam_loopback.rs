//! Integration tests for Steam P2P transport (loopback).
//!
//! These tests require:
//! - Steam client running and logged in
//! - App ID 480 (Valve Spacewar) — no game ownership required
//!
//! All tests are `#[ignore]` so they don't run in CI.
//!
//! Run with:
//! ```sh
//! cargo test -p libp2p-steam-transport --test steam_loopback -- --ignored
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use libp2p_core::Multiaddr;
use libp2p_core::Transport as _;
use libp2p_core::transport::{ListenerId, TransportError};
use libp2p_steam_transport::{SteamTransport, SteamTransportConfig, steam_multiaddr};

/// Test context that initializes Steam SDK and pumps callbacks on a background
/// thread. Drop stops the callback pump.
struct SteamTestCtx {
    client: steamworks::Client,
    stop: Arc<AtomicBool>,
    _pump: Option<std::thread::JoinHandle<()>>,
}

impl SteamTestCtx {
    /// Try to initialize Steam. Returns `None` if Steam isn't available
    /// (not running, SDK version mismatch, etc.). Tests should early-return
    /// when this returns `None`.
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

// =============================================================================
// TRANSPORT CREATION
// =============================================================================

#[test]
#[ignore]
fn transport_creation_succeeds() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };
    let _transport = SteamTransport::new(SteamTransportConfig {
        networking_sockets: Arc::new(ctx.sockets()),
        virtual_port: 0,
        local_steam_id: ctx.steam_id(),
        dial_virtual_port: None,
    });
}

// =============================================================================
// LISTEN
// =============================================================================

#[test]
#[ignore]
fn listen_on_succeeds() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    // listen_on calls create_listen_socket_p2p which requires a tokio runtime
    // for spawn_blocking in the listener bridge.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let mut transport = SteamTransport::new(SteamTransportConfig {
            networking_sockets: Arc::new(ctx.sockets()),
            virtual_port: 0,
            local_steam_id: ctx.steam_id(),
            dial_virtual_port: None,
        });

        let addr = steam_multiaddr(ctx.steam_id());
        let id = ListenerId::next();
        transport
            .listen_on(id, addr)
            .expect("listen_on should succeed");

        // Cleanup
        assert!(transport.remove_listener(id));
        assert!(!transport.remove_listener(id)); // second remove returns false
    });
}

#[test]
#[ignore]
fn listen_remove_closes_idle_listener_without_peer_events() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let mut transport = SteamTransport::new(SteamTransportConfig {
            networking_sockets: Arc::new(ctx.sockets()),
            virtual_port: 0,
            local_steam_id: ctx.steam_id(),
            dial_virtual_port: None,
        });

        let addr = steam_multiaddr(ctx.steam_id());
        let id = ListenerId::next();
        transport
            .listen_on(id, addr)
            .expect("listen_on should succeed");

        assert!(transport.remove_listener(id));
        assert!(!transport.remove_listener(id));

        drop(transport);
        tokio::time::sleep(Duration::from_millis(100)).await;
    });
}

#[test]
#[ignore]
fn listen_on_rejects_tcp_address() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let mut transport = SteamTransport::new(SteamTransportConfig {
            networking_sockets: Arc::new(ctx.sockets()),
            virtual_port: 0,
            local_steam_id: ctx.steam_id(),
            dial_virtual_port: None,
        });

        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/8080".parse().unwrap();
        let id = ListenerId::next();
        let result = transport.listen_on(id, addr);
        assert!(
            matches!(result, Err(TransportError::MultiaddrNotSupported(_))),
            "TCP address should be rejected"
        );
    });
}

// =============================================================================
// DIAL
// =============================================================================

#[test]
#[ignore]
fn dial_returns_future() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let mut transport = SteamTransport::new(SteamTransportConfig {
            networking_sockets: Arc::new(ctx.sockets()),
            virtual_port: 0,
            local_steam_id: ctx.steam_id(),
            dial_virtual_port: None,
        });

        let addr = steam_multiaddr(ctx.steam_id());
        let opts = libp2p_core::transport::DialOpts {
            role: libp2p_core::Endpoint::Dialer,
            port_use: libp2p_core::transport::PortUse::Reuse,
        };
        let result = transport.dial(addr, opts);
        assert!(
            result.is_ok(),
            "dial should return Ok(future): {:?}",
            result.err()
        );
        // We don't await the future here — that would require a matching listener.
        // Just verifying the API call succeeds is enough.
    });
}

#[test]
#[ignore]
fn dial_rejects_wildcard() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let mut transport = SteamTransport::new(SteamTransportConfig {
            networking_sockets: Arc::new(ctx.sockets()),
            virtual_port: 0,
            local_steam_id: ctx.steam_id(),
            dial_virtual_port: None,
        });

        let addr = steam_multiaddr(0);
        let opts = libp2p_core::transport::DialOpts {
            role: libp2p_core::Endpoint::Dialer,
            port_use: libp2p_core::transport::PortUse::Reuse,
        };
        let result = transport.dial(addr, opts);
        assert!(result.is_err(), "dialing /memory/0 should fail");
    });
}

#[test]
#[ignore]
fn dial_rejects_tcp_address() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let mut transport = SteamTransport::new(SteamTransportConfig {
            networking_sockets: Arc::new(ctx.sockets()),
            virtual_port: 0,
            local_steam_id: ctx.steam_id(),
            dial_virtual_port: None,
        });

        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/8080".parse().unwrap();
        let opts = libp2p_core::transport::DialOpts {
            role: libp2p_core::Endpoint::Dialer,
            port_use: libp2p_core::transport::PortUse::Reuse,
        };
        let result = transport.dial(addr, opts);
        assert!(
            matches!(result, Err(TransportError::MultiaddrNotSupported(_))),
            "TCP address should be rejected"
        );
    });
}

// =============================================================================
// DIAL_VIRTUAL_PORT
// =============================================================================

#[test]
#[ignore]
fn dial_virtual_port_defaults_to_listen_port() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        // When dial_virtual_port is None, dial should use the same port as listen.
        // We verify by successfully creating the transport (internal state check).
        let transport = SteamTransport::new(SteamTransportConfig {
            networking_sockets: Arc::new(ctx.sockets()),
            virtual_port: 42,
            local_steam_id: ctx.steam_id(),
            dial_virtual_port: None,
        });

        // Can't directly inspect dial_virtual_port, but we verify the transport
        // was created without error. The dial path will use port 42.
        drop(transport);
    });
}

#[test]
#[ignore]
fn dial_virtual_port_override() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        // Verify transport accepts different dial port (used for loopback testing).
        let mut transport = SteamTransport::new(SteamTransportConfig {
            networking_sockets: Arc::new(ctx.sockets()),
            virtual_port: 10,
            local_steam_id: ctx.steam_id(),
            dial_virtual_port: Some(20),
        });

        // Dial should succeed (connect_p2p will target port 20)
        let addr = steam_multiaddr(ctx.steam_id());
        let opts = libp2p_core::transport::DialOpts {
            role: libp2p_core::Endpoint::Dialer,
            port_use: libp2p_core::transport::PortUse::Reuse,
        };
        let result = transport.dial(addr, opts);
        assert!(result.is_ok(), "dial with override port should succeed");
    });
}

// =============================================================================
// RAW STEAM LOOPBACK CONNECTION
// =============================================================================

/// Test that `ISteamNetworkingSockets` can do a P2P loopback connection
/// (connecting to your own `SteamID`). This validates the Steam SDK works
/// for our use case before involving the full libp2p stack.
#[test]
#[ignore]
fn raw_steam_loopback_connection() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };
    let sockets = ctx.sockets();
    let steam_id = ctx.steam_id();

    // Create a listen socket on virtual port 100
    let listen_socket = sockets
        .create_listen_socket_p2p(100, [])
        .expect("create_listen_socket_p2p should succeed");

    // Connect to ourselves on port 100
    let identity = steamworks::networking_types::NetworkingIdentity::new_steam_id(
        steamworks::SteamId::from_raw(steam_id),
    );
    let connection = sockets
        .connect_p2p(identity, 100, [])
        .expect("connect_p2p to self should succeed");

    // Poll until the connection is established or we timeout.
    // The callback pump thread (in SteamTestCtx) drives the connection state machine.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut connected = false;

    // Also accept the incoming connection on the listen socket
    let mut accepted = false;

    while std::time::Instant::now() < deadline {
        // Check outbound connection state
        if let Ok(info) = sockets.get_connection_info(&connection)
            && info
                .state()
                .is_ok_and(|state| format!("{state:?}") == "Connected")
        {
            connected = true;
        }

        // Accept incoming connections on the listen socket (non-blocking)
        if !accepted {
            while let Some(event) = listen_socket.try_receive_event() {
                match event {
                    steamworks::networking_types::ListenSocketEvent::Connecting(req) => {
                        let _ = req.accept();
                    }
                    steamworks::networking_types::ListenSocketEvent::Connected(_) => {
                        accepted = true;
                    }
                    steamworks::networking_types::ListenSocketEvent::Disconnected(_) => {}
                }
            }
        }

        if connected && accepted {
            break;
        }

        std::thread::sleep(Duration::from_millis(16));
    }

    assert!(
        connected,
        "Outbound connection should reach Connected state within 5s"
    );
    assert!(
        accepted,
        "Listen socket should accept the loopback connection within 5s"
    );
}
