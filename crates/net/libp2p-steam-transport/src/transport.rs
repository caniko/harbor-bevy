use std::collections::{HashMap, VecDeque};
use std::future::{Future, Ready, ready};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use libp2p_core::Multiaddr;
use libp2p_core::transport::{ListenerId, TransportError, TransportEvent};
use steamworks::SteamId;
use steamworks::networking_sockets::{NetConnection, NetworkingSockets};
use steamworks::networking_types::NetworkingIdentity;

use crate::dial;
use crate::error::SteamTransportError;
use crate::listener;
use crate::protocol;
use crate::stream::SteamStream;

/// Validate a listen address. Returns `Ok(parsed_steam_id)` or `Err(MultiaddrNotSupported)`.
pub(crate) fn validate_listen_addr(
    addr: &Multiaddr,
) -> Result<u64, TransportError<SteamTransportError>> {
    protocol::parse_steam_id(addr)
        .ok_or_else(|| TransportError::MultiaddrNotSupported(addr.clone()))
}

/// Validate a dial address. Returns `Ok(remote_steam_id)` or an appropriate error.
///
/// Rejects wildcard `/memory/0` (can't dial nobody) and non-steam multiaddrs.
pub(crate) fn validate_dial_addr(
    addr: &Multiaddr,
) -> Result<u64, TransportError<SteamTransportError>> {
    let id = protocol::parse_steam_id(addr)
        .ok_or_else(|| TransportError::MultiaddrNotSupported(addr.clone()))?;
    if id == 0 {
        return Err(TransportError::Other(SteamTransportError::ConnectFailed(
            "cannot dial wildcard address /memory/0".into(),
        )));
    }
    Ok(id)
}

/// Configuration for the Steam relay transport.
pub struct SteamTransportConfig {
    /// The `NetworkingSockets` handle from the Steam client.
    /// Wrapped in `Arc` so it can be shared between the transport and muxer.
    pub networking_sockets: Arc<NetworkingSockets>,
    /// Virtual port for P2P listen sockets (typically 0).
    pub virtual_port: i32,
    /// The local user's Steam ID (for constructing listen addresses).
    pub local_steam_id: u64,
    /// Virtual port to use when dialing remote peers.
    /// If `None`, uses `virtual_port` for both listen and dial.
    /// Set this to a different value for loopback testing (same `SteamID` on both ends).
    pub dial_virtual_port: Option<i32>,
}

/// libp2p Transport over Steam Relay using `ISteamNetworkingSockets`.
///
/// Uses `spawn_blocking` bridges to convert Steam's callback-driven
/// `ListenSocket` events into async-compatible `tokio::sync::mpsc` channels.
/// `Transport::poll()` consumes 0 CPU when idle.
pub struct SteamTransport {
    sockets: Arc<NetworkingSockets>,
    listeners: HashMap<ListenerId, listener::SteamListener>,
    pending_events: VecDeque<
        TransportEvent<Ready<Result<SteamStream, SteamTransportError>>, SteamTransportError>,
    >,
    virtual_port: i32,
    dial_virtual_port: i32,
    local_steam_id: u64,
}

impl SteamTransport {
    pub fn new(config: SteamTransportConfig) -> Self {
        let dial_port = config.dial_virtual_port.unwrap_or(config.virtual_port);
        Self {
            sockets: config.networking_sockets,
            listeners: HashMap::new(),
            pending_events: VecDeque::new(),
            virtual_port: config.virtual_port,
            dial_virtual_port: dial_port,
            local_steam_id: config.local_steam_id,
        }
    }

    fn make_stream(&self, connection: NetConnection) -> SteamStream {
        SteamStream::new(connection)
    }
}

impl libp2p_core::Transport for SteamTransport {
    type Output = SteamStream;
    type Error = SteamTransportError;
    type Dial = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;
    type ListenerUpgrade = Ready<Result<Self::Output, Self::Error>>;

    fn listen_on(
        &mut self,
        id: ListenerId,
        addr: Multiaddr,
    ) -> Result<(), TransportError<Self::Error>> {
        validate_listen_addr(&addr)?;

        let listen_socket = self
            .sockets
            .create_listen_socket_p2p(self.virtual_port, [])
            .map_err(|_| TransportError::Other(SteamTransportError::ListenFailed))?;

        let rx = listener::spawn_listener_bridge(listen_socket);
        self.listeners.insert(id, rx);

        // Queue a NewAddress event so the swarm knows the listen address
        let listen_addr = protocol::steam_multiaddr(self.local_steam_id);
        self.pending_events.push_back(TransportEvent::NewAddress {
            listener_id: id,
            listen_addr,
        });

        log::info!(
            "Steam transport: listening on virtual port {} (listener {id:?})",
            self.virtual_port
        );

        Ok(())
    }

    fn remove_listener(&mut self, id: ListenerId) -> bool {
        if let Some(listener) = self.listeners.remove(&id) {
            listener.shutdown();
            log::debug!("Steam transport: removed listener {id:?}");
            self.pending_events
                .push_back(TransportEvent::ListenerClosed {
                    listener_id: id,
                    reason: Ok(()),
                });
            true
        } else {
            false
        }
    }

    fn dial(
        &mut self,
        addr: Multiaddr,
        _opts: libp2p_core::transport::DialOpts,
    ) -> Result<Self::Dial, TransportError<Self::Error>> {
        let remote_steam_id = validate_dial_addr(&addr)?;

        let identity = NetworkingIdentity::new_steam_id(SteamId::from_raw(remote_steam_id));

        let connection = self
            .sockets
            .connect_p2p(identity, self.dial_virtual_port, [])
            .map_err(|_| {
                TransportError::Other(SteamTransportError::ConnectFailed(
                    "connect_p2p failed".into(),
                ))
            })?;

        log::info!(
            "Steam transport: dialing steam_id={remote_steam_id} on virtual port {}",
            self.dial_virtual_port
        );

        let rx = dial::spawn_dial(self.sockets.clone(), connection);

        Ok(Box::pin(async move {
            match rx.await {
                Ok(Ok(conn)) => Ok(SteamStream::new(conn)),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(SteamTransportError::DialCancelled),
            }
        }))
    }

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<TransportEvent<Self::ListenerUpgrade, Self::Error>> {
        let this = self.get_mut();

        // 1. Drain any pre-queued events (NewAddress, ListenerClosed)
        if let Some(event) = this.pending_events.pop_front() {
            return Poll::Ready(event);
        }

        // 2. Poll all listener receivers for incoming connections
        let mut dead_listeners = Vec::new();
        for (&listener_id, listener) in &mut this.listeners {
            match listener.poll_recv(cx) {
                Poll::Ready(Some(connection)) => {
                    let stream = this.make_stream(connection);
                    let local_addr = protocol::steam_multiaddr(this.local_steam_id);
                    // We don't know the remote Steam ID from the NetConnection
                    // directly, so use a placeholder. The Noise handshake will
                    // establish the real PeerId.
                    let send_back_addr = protocol::steam_multiaddr(0);

                    return Poll::Ready(TransportEvent::Incoming {
                        listener_id,
                        upgrade: ready(Ok(stream)),
                        local_addr,
                        send_back_addr,
                    });
                }
                Poll::Ready(None) => {
                    // Bridge task exited (ListenSocket closed) — mark for removal
                    dead_listeners.push(listener_id);
                    this.pending_events
                        .push_back(TransportEvent::ListenerClosed {
                            listener_id,
                            reason: Ok(()),
                        });
                }
                Poll::Pending => {}
            }
        }

        // Remove dead listeners so they aren't polled again
        for id in dead_listeners {
            this.listeners.remove(&id);
        }

        // 3. Drain events generated during listener polling
        if let Some(event) = this.pending_events.pop_front() {
            return Poll::Ready(event);
        }

        Poll::Pending
    }
}
