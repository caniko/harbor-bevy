//! `Thespan` Bevy: Bevy integration for thespan networking.
//!
//! Provides connection lifecycle management and event polling for thespan.
//! Applications can dispatch messages via piying remote actors.
#![allow(clippy::result_large_err)]

pub mod poll;
pub mod resource;

use bevy::prelude::*;
use veritasium_wire::PeerIdBytes;

// ---------------------------------------------------------------------------
// Re-exports from thespan (transport layer)
// ---------------------------------------------------------------------------

#[cfg(feature = "legacy-raw-codec")]
pub use thespan::SendTarget;
#[cfg(feature = "steam")]
pub use thespan::SteamConfig;
pub use thespan::{
    CommandHandle, CommandSendError, DIRECT_SESSION_MAX_BYTES, EventReceiver,
    GAMEPLAY_MESSAGE_MAX_BYTES, GOSSIPSUB_MAX_TRANSMIT_SIZE, InternetConfig, LanConfig, Multiaddr,
    NetworkCommand, PreRunHook, PreRunWithCleanup, STATE_SYNC_MESSAGE_MAX_BYTES, SwarmSender,
    ThespanConfig, TransportError, TransportMessageKind, TransportType, WebRtcConfig,
    ed25519_public_key_from_peer_id, identity,
};

// Re-export libp2p PeerId for public API
pub use thespan::PeerId;

// The WebRTC listener certificate generator, so the persistence layer can keep
// a listener's `/certhash/...` stable across restarts.
pub use thespan::transport::webrtc::generate_certificate_pem;

// ---------------------------------------------------------------------------
// Network event wrapper types
// ---------------------------------------------------------------------------

/// Wrapper for events received from the network via piying remote messaging.
///
/// Triggered by `poll_session_events` after piying dispatches a remote message.
/// Receive bridges observe `On<FromNetwork<T>>` and re-trigger as local `T`.
#[derive(Event, Clone)]
pub struct FromNetwork<T: Event + Clone> {
    pub inner: T,
    /// Sender's libp2p `PeerId` when available.
    pub from: Option<PeerIdBytes>,
}

// Re-exports from resource
#[cfg(feature = "legacy-raw-codec")]
pub use resource::RawMessageEvent;
pub use resource::{
    GossipSubscribeFailedEvent, GossipSubscribedEvent, KeepAlivePeerTelemetry, KeepAliveTelemetry,
    ListenStartedEvent, NetworkResource, OversizedMessageDrops, PeerConnectedEvent,
    PeerDisconnectedEvent, RawGossipMessageEvent, RemoteSendFailedEvent, ShutdownDrains,
    TransportConnectionStatus,
};

/// Bevy plugin that sets up the `thespan_bevy` networking infrastructure.
///
/// Registers `poll_network_events` system to handle connection lifecycle events.
/// Application message dispatch happens via piying remote actors.
pub struct ThespanBevyPlugin;

impl Plugin for ThespanBevyPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<KeepAliveTelemetry>();
        app.init_resource::<OversizedMessageDrops>();
        app.add_systems(
            PreUpdate,
            (
                poll::poll_network_events.run_if(resource_exists::<NetworkResource>),
                poll::sync_keep_alive_telemetry
                    .after(poll::poll_network_events)
                    .run_if(resource_exists::<NetworkResource>),
            ),
        );
        app.add_systems(
            PreUpdate,
            poll::clear_keep_alive_telemetry.run_if(not(resource_exists::<NetworkResource>)),
        );
    }
}

/// Spawn the background network actor and return a `NetworkResource`.
///
/// The actor runs in a dedicated thread with its own tokio runtime.
/// Insert the returned resource into Bevy:
/// ```ignore
/// let resource = thespan_bevy::spawn_network_actor(config);
/// commands.insert_resource(resource);
/// ```
pub fn spawn_network_actor(config: TransportType) -> Result<NetworkResource, TransportError> {
    spawn_network_actor_with_hook(config, None)
}

/// Like [`spawn_network_actor`] but accepts a pre-run hook that executes inside
/// the tokio runtime context before the network actor starts blocking.
///
/// The hook receives the local `PeerId` so it can register actors under
/// well-known IDs for direct remote references (bypassing DHT lookup).
///
/// Use this to spawn additional piying actors on the same runtime.
pub fn spawn_network_actor_with_hook(
    config: TransportType,
    pre_run_hook: PreRunHook,
) -> Result<NetworkResource, TransportError> {
    spawn_network_actor_with_config(config, ThespanConfig::default(), None, pre_run_hook)
}

/// Like [`spawn_network_actor_with_hook`] but uses a caller-supplied libp2p
/// identity instead of loading the persistent identity from disk.
pub fn spawn_network_actor_with_keypair(
    config: TransportType,
    keypair: identity::Keypair,
    pre_run_hook: PreRunHook,
) -> Result<NetworkResource, TransportError> {
    spawn_network_actor_with_config(
        config,
        ThespanConfig::default(),
        Some(keypair),
        pre_run_hook,
    )
}

/// Spawn with a custom wire prefix and persistent identity location.
///
/// `keypair` overrides the configured persistent identity when supplied.
pub fn spawn_network_actor_with_config(
    config: TransportType,
    thespan_config: ThespanConfig,
    keypair: Option<identity::Keypair>,
    pre_run_hook: PreRunHook,
) -> Result<NetworkResource, TransportError> {
    let handle =
        thespan::spawn_network_actor_with_config(config, thespan_config, keypair, pre_run_hook)?
            .into_parts();
    Ok(network_resource_from_parts(handle))
}

/// Spawn with cleanup awaited on the transport thread before its runtime exits.
pub fn spawn_with_lifecycle(
    config: TransportType,
    thespan_config: ThespanConfig,
    keypair: Option<identity::Keypair>,
    pre_run_hook: Option<PreRunWithCleanup>,
) -> Result<NetworkResource, TransportError> {
    thespan::spawn_with_lifecycle(config, thespan_config, keypair, pre_run_hook)
        .map(|handle| network_resource_from_parts(handle.into_parts()))
}

fn network_resource_from_parts(handle: thespan::handle::NetworkHandleParts) -> NetworkResource {
    NetworkResource {
        command: handle.command,
        event_rx: handle.event_rx,
        local_peer_id: handle.local_peer_id,
        identity_public_key: handle.identity_public_key,
        identity_keypair: handle.identity_keypair,
        gossipsub_enabled: handle.gossipsub_enabled,
        swarm_sender: handle.swarm_sender,
        keep_alive_telemetry: handle.keep_alive_telemetry,
        state: TransportConnectionStatus::Disconnected,
        peers: std::collections::HashSet::new(),
        peer_addrs: std::collections::HashMap::new(),
        listen_addr: None,
        thread_handle: handle.thread_handle,
        shutdown_drains: ShutdownDrains::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_spawn_passes_the_wire_prefix_to_the_transport() {
        let result = spawn_network_actor_with_config(
            TransportType::Lan(LanConfig::default()),
            ThespanConfig::new("example", "missing-leading-slash", "EXAMPLE_IDENTITY_DIR"),
            Some(identity::Keypair::generate_ed25519()),
            None,
        );
        assert!(matches!(
            result,
            Err(TransportError::Behaviour(message)) if message.contains("forward slash")
        ));
    }
}
