// intentionally oversize: release-gate baseline; split requires scoped semantic extraction.
//! Spawning the background network actor.

use std::collections::HashSet;

use libp2p::{Multiaddr, PeerId, identity::Keypair, swarm::Swarm};
use piying::actor::Spawn;

use crate::actor::behavior::{KeepAliveTelemetryHandle, ThespanBehaviour};
use crate::actor::network::{NetworkActor, NetworkActorArgs};
use crate::config::{ThespanConfig, TransportType};
use crate::handle::{CommandHandle, NetworkHandle};
use crate::transport::TransportError;

pub const THESPAN_EVENT_CHANNEL_CAPACITY: usize = 4096;

/// Type alias for the optional pre-run hook passed to network actor spawn functions.
///
/// The hook runs inside the tokio runtime context before the actor starts blocking,
/// receiving the local `PeerId` so it can register actors under well-known IDs.
pub type PreRunHook = Option<Box<dyn FnOnce(libp2p::PeerId) + Send>>;

type BootstrapResult =
    Result<(piying::remote::SwarmSender, KeepAliveTelemetryHandle), TransportError>;
type SwarmSetup = Result<
    (
        Swarm<ThespanBehaviour>,
        Multiaddr,
        Vec<Multiaddr>,
        HashSet<PeerId>,
        Option<Multiaddr>,
    ),
    TransportError,
>;

fn event_channel() -> (
    crossbeam_channel::Sender<crate::actor::events::ActorEvent>,
    crossbeam_channel::Receiver<crate::actor::events::ActorEvent>,
) {
    crossbeam_channel::bounded(THESPAN_EVENT_CHANNEL_CAPACITY)
}

#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub fn test_fill_event_channel_without_consumer(total: usize) -> u64 {
    let (tx, _rx) = event_channel();
    let before = crate::handle::THESPAN_EVENT_DROPS.load(std::sync::atomic::Ordering::Relaxed);
    for _ in 0..total {
        let event = crate::actor::events::ActorEvent::ListenStarted {
            addr: "/memory/0".parse().expect("valid memory multiaddr"),
        };
        if tx.try_send(event).is_err() {
            crate::handle::THESPAN_EVENT_DROPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    crate::handle::THESPAN_EVENT_DROPS
        .load(std::sync::atomic::Ordering::Relaxed)
        .saturating_sub(before)
}

#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub fn test_bootstrap_error_round_trip() -> Result<(), TransportError> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let thread_handle = std::thread::Builder::new()
        .name("thespan-bootstrap-error-test".into())
        .spawn(move || {
            let _ = tx.send(Err(TransportError::Behaviour(
                "injected bootstrap failure".into(),
            )));
        })
        .map_err(|e| TransportError::Transport(format!("spawn bootstrap test thread: {e}")))?;
    receive_bootstrap(rx, thread_handle).map(|_| ())
}

fn receive_bootstrap(
    rx: std::sync::mpsc::Receiver<BootstrapResult>,
    thread_handle: std::thread::JoinHandle<()>,
) -> Result<
    (
        piying::remote::SwarmSender,
        KeepAliveTelemetryHandle,
        std::thread::JoinHandle<()>,
    ),
    TransportError,
> {
    match rx.recv() {
        Ok(Ok((swarm_sender, keep_alive_telemetry))) => {
            Ok((swarm_sender, keep_alive_telemetry, thread_handle))
        }
        Ok(Err(err)) => {
            crate::handle::join_network_thread(thread_handle);
            Err(err)
        }
        Err(_recv_err) => {
            crate::handle::join_network_thread(thread_handle);
            Err(TransportError::Transport(
                "network thread exited before initialization".into(),
            ))
        }
    }
}

fn install_panic_hook_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            tracing::error!(
                target: "thespan::panics",
                payload = %info,
                "thespan runtime panic"
            );
            default_hook(info);
        }));
    });
}

fn ed25519_public_key_bytes(keypair: &libp2p::identity::Keypair) -> Option<[u8; 32]> {
    keypair
        .public()
        .try_into_ed25519()
        .ok()
        .map(|public| public.to_bytes())
}

/// Recover the Ed25519 public key bytes embedded in a libp2p [`PeerId`].
///
/// This lets an application compare a signing key with the one libp2p
/// authenticated for the peer. Hashed or non-Ed25519 `PeerId`s return `None`.
///
/// libp2p 0.56 does not expose a dedicated `PeerId` → `PublicKey` inverse. To
/// avoid depending on a hand-parsed multihash layout, this decodes only the
/// candidate inline key bytes through libp2p's public-key decoder and then
/// re-derives the `PeerId`. The binding is accepted only if that round-trip
/// exactly reproduces the authenticated `PeerId`.
pub fn ed25519_public_key_from_peer_id(peer_id: &libp2p::PeerId) -> Option<[u8; 32]> {
    let public_key =
        libp2p::identity::PublicKey::try_decode_protobuf(peer_id.as_ref().digest()).ok()?;
    if public_key.to_peer_id() != *peer_id {
        return None;
    }
    public_key
        .try_into_ed25519()
        .ok()
        .map(|public| public.to_bytes())
}

/// Spawn the background network actor and return a [`NetworkHandle`].
///
/// Transports run as a piying Actor on a dedicated thread with their own tokio
/// runtime. LAN uses QUIC, WebRTC uses WebRTC Direct, and Steam uses
/// `SteamTransport` with Yamux.
///
/// Call `spawn_network_actor(config)` and use the returned [`NetworkHandle`] to
/// send commands and receive events.
pub fn spawn_network_actor(config: TransportType) -> Result<NetworkHandle, TransportError> {
    spawn_network_actor_with_hook(config, None)
}

/// Like [`spawn_network_actor`] but accepts an optional hook that runs inside
/// the tokio runtime context *before* the network actor starts blocking.
///
/// The hook receives the local `PeerId` so it can register actors under
/// well-known IDs for direct remote references (bypassing DHT lookup).
///
/// Use this to spawn additional piying actors on the same runtime.
pub fn spawn_network_actor_with_hook(
    config: TransportType,
    pre_run_hook: PreRunHook,
) -> Result<NetworkHandle, TransportError> {
    spawn_network_actor_with_config(config, ThespanConfig::default(), None, pre_run_hook)
}

/// Like [`spawn_network_actor_with_hook`] but uses a caller-supplied identity.
pub fn spawn_network_actor_with_keypair(
    config: TransportType,
    keypair: libp2p::identity::Keypair,
    pre_run_hook: PreRunHook,
) -> Result<NetworkHandle, TransportError> {
    spawn_network_actor_with_config(
        config,
        ThespanConfig::default(),
        Some(keypair),
        pre_run_hook,
    )
}

/// Spawn using the application's wire prefix and identity location.
///
/// Use `None` to load or create the persistent identity configured in
/// `thespan_config`. LAN, WebRTC, and Internet still get their request-response
/// IDs from the transport config, independent of the keep-alive prefix.
pub fn spawn_network_actor_with_config(
    config: TransportType,
    thespan_config: ThespanConfig,
    keypair: Option<Keypair>,
    pre_run_hook: PreRunHook,
) -> Result<NetworkHandle, TransportError> {
    let keypair = match keypair {
        Some(keypair) => keypair,
        None => thespan_config
            .load_or_create_keypair()
            .map_err(|e| TransportError::Transport(format!("load persistent identity: {e}")))?,
    };
    install_panic_hook_once();
    match config {
        TransportType::Lan(lan_config) => {
            spawn_lan_actor(lan_config, thespan_config, keypair, pre_run_hook)
        }
        TransportType::WebRtc(webrtc_config) => {
            spawn_webrtc_actor(webrtc_config, thespan_config, keypair, pre_run_hook)
        }
        TransportType::Internet(internet_config) => {
            spawn_internet_actor(internet_config, thespan_config, keypair, pre_run_hook)
        }
        #[cfg(feature = "steam")]
        TransportType::Steam(steam_config) => {
            spawn_steam_actor(steam_config, thespan_config, keypair, pre_run_hook)
        }
    }
}

fn spawn_actor(
    name: &'static str,
    local_peer_id: PeerId,
    identity_public_key: Option<[u8; 32]>,
    identity_keypair: Option<Keypair>,
    gossipsub_enabled: bool,
    pre_run_hook: PreRunHook,
    setup: impl FnOnce() -> SwarmSetup + Send + 'static,
) -> Result<NetworkHandle, TransportError> {
    let (evt_tx, evt_rx) = event_channel();
    let prepared = NetworkActor::prepare_with_mailbox(piying::mailbox::unbounded());
    let actor_ref = prepared.actor_ref().clone();
    let (swarm_sender_tx, swarm_sender_rx) = std::sync::mpsc::sync_channel(1);

    let thread_handle = std::thread::Builder::new()
        .name(format!("thespan-{name}"))
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to create thespan tokio runtime");

            rt.block_on(async move {
                if let Some(hook) = pre_run_hook {
                    hook(local_peer_id);
                }

                let (swarm, listen_addr, additional_listen_addrs, infrastructure_peers, dial_addr) =
                    match setup() {
                        Ok(setup) => setup,
                        Err(error) => {
                            let _ = swarm_sender_tx.send(Err(error));
                            return;
                        }
                    };
                let args = NetworkActorArgs {
                    swarm,
                    listen_addr,
                    additional_listen_addrs,
                    event_tx: evt_tx,
                    bootstrap_tx: Some(swarm_sender_tx),
                    infrastructure_peers,
                    dial_addr,
                };
                match prepared.run(args).await {
                    Ok((_actor, reason)) => tracing::debug!(
                        target: "thespan::lifecycle",
                        transport = name,
                        ?reason,
                        "network actor stopped"
                    ),
                    Err(error) => tracing::error!(
                        target: "thespan::lifecycle",
                        transport = name,
                        ?error,
                        "network actor stopped with error"
                    ),
                }
            });
            tracing::debug!(
                target: "thespan::lifecycle",
                transport = name,
                "network runtime exited"
            );
        })
        .map_err(|error| {
            TransportError::Transport(format!("spawn {name} network thread: {error}"))
        })?;
    let (swarm_sender, keep_alive_telemetry, thread_handle) =
        receive_bootstrap(swarm_sender_rx, thread_handle)?;

    Ok(NetworkHandle {
        command: CommandHandle::from_piying(actor_ref),
        event_rx: evt_rx,
        local_peer_id,
        identity_public_key,
        identity_keypair,
        gossipsub_enabled,
        swarm_sender,
        keep_alive_telemetry,
        thread_handle: Some(thread_handle),
    })
}

fn spawn_internet_actor(
    config: crate::transport::libp2p_internet::InternetConfig,
    thespan_config: ThespanConfig,
    keypair: Keypair,
    pre_run_hook: PreRunHook,
) -> Result<NetworkHandle, TransportError> {
    use crate::transport::libp2p_internet;

    let local_peer_id = keypair.public().to_peer_id();
    let identity_public_key = ed25519_public_key_bytes(&keypair);
    let identity_keypair = keypair.clone();
    let gossipsub_enabled = config.gossipsub_enabled;
    spawn_actor(
        "internet",
        local_peer_id,
        identity_public_key,
        Some(identity_keypair),
        gossipsub_enabled,
        pre_run_hook,
        move || {
            let swarm = libp2p_internet::build_internet_swarm_with_config(
                &config,
                keypair,
                &thespan_config,
            )?;
            Ok((
                swarm,
                libp2p_internet::quic_listen_addr(&config)?,
                libp2p_internet::additional_listen_addrs(&config)?,
                libp2p_internet::infrastructure_peer_ids(&config),
                config.dial_addr,
            ))
        },
    )
}

fn spawn_lan_actor(
    config: crate::config::LanConfig,
    thespan_config: ThespanConfig,
    keypair: Keypair,
    pre_run_hook: PreRunHook,
) -> Result<NetworkHandle, TransportError> {
    let local_peer_id = keypair.public().to_peer_id();
    let identity_public_key = ed25519_public_key_bytes(&keypair);
    let identity_keypair = keypair.clone();
    let gossipsub_enabled = config.gossipsub_enabled;
    spawn_actor(
        "lan",
        local_peer_id,
        identity_public_key,
        Some(identity_keypair),
        gossipsub_enabled,
        pre_run_hook,
        move || {
            let swarm = crate::transport::lan::build_lan_swarm_with_keypair(
                &config,
                keypair,
                &thespan_config,
            )?;
            Ok((
                swarm,
                crate::transport::lan::listen_addr(&config)?,
                Vec::new(),
                HashSet::new(),
                config.dial_addr,
            ))
        },
    )
}

fn spawn_webrtc_actor(
    config: crate::config::WebRtcConfig,
    thespan_config: ThespanConfig,
    keypair: Keypair,
    pre_run_hook: PreRunHook,
) -> Result<NetworkHandle, TransportError> {
    let local_peer_id = keypair.public().to_peer_id();
    let identity_public_key = ed25519_public_key_bytes(&keypair);
    let identity_keypair = keypair.clone();
    let gossipsub_enabled = config.gossipsub_enabled;
    spawn_actor(
        "webrtc",
        local_peer_id,
        identity_public_key,
        Some(identity_keypair),
        gossipsub_enabled,
        pre_run_hook,
        move || {
            let swarm = crate::transport::webrtc::build_webrtc_swarm_with_keypair(
                &config,
                keypair,
                &thespan_config,
            )?;
            Ok((
                swarm,
                crate::transport::webrtc::listen_addr(&config)?,
                Vec::new(),
                HashSet::new(),
                config.dial_addr,
            ))
        },
    )
}

#[cfg(feature = "steam")]
fn spawn_steam_actor(
    config: crate::config::SteamConfig,
    thespan_config: ThespanConfig,
    keypair: Keypair,
    pre_run_hook: PreRunHook,
) -> Result<NetworkHandle, TransportError> {
    use crate::transport::steam;
    let local_peer_id = keypair.public().to_peer_id();
    let identity_public_key = ed25519_public_key_bytes(&keypair);
    let identity_keypair = keypair.clone();
    let local_steam_id = config.local_steam_id;
    spawn_actor(
        "steam",
        local_peer_id,
        identity_public_key,
        Some(identity_keypair),
        false,
        pre_run_hook,
        move || {
            let swarm = steam::build_steam_swarm_with_keypair(
                config.networking_sockets,
                config.virtual_port,
                local_steam_id,
                keypair,
                config.remote_virtual_port,
                &thespan_config,
            )
            .map_err(|error| match error {
                steam::SteamSwarmError::Transport(message) => TransportError::Transport(message),
                steam::SteamSwarmError::Behaviour(message) => TransportError::Behaviour(message),
            })?;
            Ok((
                swarm,
                steam::listen_addr(local_steam_id),
                Vec::new(),
                HashSet::new(),
                config.remote_steam_id.map(steam::dial_addr),
            ))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ed25519_public_key_from_peer_id_round_trips_identity_key() {
        for _ in 0..100 {
            let keypair = libp2p::identity::Keypair::generate_ed25519();
            let public_key = ed25519_public_key_bytes(&keypair).expect("ed25519 key");
            let peer_id = keypair.public().to_peer_id();

            assert_eq!(ed25519_public_key_from_peer_id(&peer_id), Some(public_key));
        }
    }
}
