use bevy::{prelude::*, state::app::StatesPlugin};
use thespan_bevy::NetworkResource;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, States)]
pub enum ConnectionPhase {
    #[default]
    Offline,
    Hosting,
    Joining,
    Negotiating,
    Connected,
    InGame,
    InTournament,
    Disconnecting,
}

impl ConnectionPhase {
    pub const fn is_in_session(self) -> bool {
        matches!(self, Self::Connected | Self::InGame | Self::InTournament)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, States)]
pub enum TransportHealth {
    #[default]
    Inactive,
    Healthy,
    Reconnecting,
    Rejoining,
    Disconnecting,
    Lost,
}

#[derive(Debug, Event)]
pub struct SessionSetupRequested;

#[derive(Debug, Event)]
pub struct SessionCleanupRequested;

#[derive(Debug, Event)]
pub struct SessionResumeRequested;

#[derive(Debug, Clone, PartialEq)]
pub enum DisconnectReason {
    PlayerLeft,
    Timeout,
    Error(String),
    InvalidMove,
    CommitmentCheat,
    TooManyTimeouts,
    Desync,
    InvariantViolation(String),
    LobbyDissolved,
    PeerReadyTimeout {
        attempts: u16,
    },
    PhaseWatchdog {
        phase: String,
        elapsed_secs: f32,
    },
    LocalInterfaceLost,
    LivenessStalled {
        missed_intervals: u8,
        elapsed_secs: f32,
    },
}

#[derive(Debug, Clone, PartialEq, Event)]
pub struct ConnectionLostEvent(pub DisconnectReason);

pub struct ConnectionLifecyclePlugin;

impl Plugin for ConnectionLifecyclePlugin {
    fn build(&self, app: &mut App) {
        use ConnectionPhase::{Connected, Hosting, Joining, Negotiating, Offline};
        use TransportHealth::{Healthy, Lost, Reconnecting, Rejoining};
        if !app.is_plugin_added::<StatesPlugin>() {
            app.add_plugins(StatesPlugin);
        }
        app.init_state::<ConnectionPhase>()
            .init_state::<TransportHealth>()
            .add_observer(disconnect_on_fault)
            .add_observer(disconnect_on_loss)
            .add_systems(
                OnTransition {
                    exited: Offline,
                    entered: Connected,
                },
                setup,
            )
            .add_systems(
                OnTransition {
                    exited: Hosting,
                    entered: Connected,
                },
                setup,
            )
            .add_systems(
                OnTransition {
                    exited: Joining,
                    entered: Connected,
                },
                setup,
            )
            .add_systems(
                OnTransition {
                    exited: Negotiating,
                    entered: Connected,
                },
                setup,
            )
            .add_systems(OnEnter(ConnectionPhase::Disconnecting), cleanup)
            .add_systems(OnEnter(TransportHealth::Disconnecting), deactivate)
            .add_systems(
                OnTransition {
                    exited: Reconnecting,
                    entered: Healthy,
                },
                resume,
            )
            .add_systems(
                OnTransition {
                    exited: Rejoining,
                    entered: Healthy,
                },
                resume,
            )
            .add_systems(
                OnTransition {
                    exited: Lost,
                    entered: Healthy,
                },
                resume,
            );
    }
}

fn disconnect_on_fault(
    fault: On<crate::SessionFault>,
    phase: Res<State<ConnectionPhase>>,
    mut next: ResMut<NextState<ConnectionPhase>>,
    mut health: ResMut<NextState<TransportHealth>>,
    mut commands: Commands,
) {
    use crate::SessionFault;
    let reason = match fault.event() {
        SessionFault::Desync => DisconnectReason::Desync,
        SessionFault::CommitmentMismatch => DisconnectReason::CommitmentCheat,
        SessionFault::CommitmentRetriesExhausted => DisconnectReason::TooManyTimeouts,
        SessionFault::ValidationViolation(description) => {
            DisconnectReason::InvariantViolation(description.clone())
        }
        other => DisconnectReason::Error(other.to_string()),
    };
    if disconnect(*phase.get(), &mut next, &mut health) {
        commands.trigger(ConnectionLostEvent(reason));
    }
}

fn disconnect_on_loss(
    _: On<ConnectionLostEvent>,
    phase: Res<State<ConnectionPhase>>,
    mut next: ResMut<NextState<ConnectionPhase>>,
    mut health: ResMut<NextState<TransportHealth>>,
) {
    disconnect(*phase.get(), &mut next, &mut health);
}

fn disconnect(
    phase: ConnectionPhase,
    next: &mut NextState<ConnectionPhase>,
    health: &mut NextState<TransportHealth>,
) -> bool {
    if matches!(
        phase,
        ConnectionPhase::Offline | ConnectionPhase::Disconnecting
    ) || matches!(next, NextState::Pending(ConnectionPhase::Disconnecting))
    {
        return false;
    }
    next.set(ConnectionPhase::Disconnecting);
    health.set(TransportHealth::Disconnecting);
    true
}

fn setup(
    mut commands: Commands,
    health: Res<State<TransportHealth>>,
    mut next: ResMut<NextState<TransportHealth>>,
) {
    if *health.get() == TransportHealth::Inactive && !matches!(*next, NextState::Pending(_)) {
        next.set(TransportHealth::Healthy);
    }
    commands.trigger(SessionSetupRequested);
}

fn resume(phase: Res<State<ConnectionPhase>>, mut commands: Commands) {
    if phase.get().is_in_session() {
        commands.trigger(SessionResumeRequested);
    }
}

fn cleanup(world: &mut World) {
    if let Some(mut network) = world.remove_resource::<NetworkResource>() {
        network.shutdown_now();
    }
    world.remove_resource::<crate::HostingState>();
    world.remove_resource::<crate::JoiningState>();
    world.remove_resource::<crate::LobbyCountdown>();
    world.trigger(SessionCleanupRequested);
    world
        .resource_mut::<NextState<ConnectionPhase>>()
        .set(ConnectionPhase::Offline);
    // A caller may request phase cleanup directly. Leave a coordinated loss
    // transition alone so health still visits Disconnecting before Inactive.
    if *world.resource::<State<TransportHealth>>().get() != TransportHealth::Disconnecting {
        let mut next = world.resource_mut::<NextState<TransportHealth>>();
        if !matches!(*next, NextState::Pending(TransportHealth::Disconnecting)) {
            next.set(TransportHealth::Inactive);
        }
    }
}

fn deactivate(mut next: ResMut<NextState<TransportHealth>>) {
    next.set(TransportHealth::Inactive);
}
