//! Degrade Steam features after an IPC disconnect.

use bevy::{log::warn, prelude::*};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SteamError {
    #[error("Steam IPC pipe disconnected")]
    PipeDisconnect,
    #[error("{0}")]
    CallFailed(String),
    #[error("Steam features are disabled: {0:?}")]
    Disabled(SteamDisableReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteamDisableReason {
    PipeDisconnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SteamState {
    #[default]
    Active,
    Disabled {
        reason: SteamDisableReason,
    },
}

#[derive(Resource, Debug, Default)]
pub struct SteamWatchdog {
    state: SteamState,
    notification_emitted: bool,
}

/// Observe this event to surface Steam degradation in an application-specific way.
#[derive(Event, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SteamFeaturesDisabledEvent {
    pub reason: SteamDisableReason,
}

/// Persistent copy of the notification for consumers which missed the event.
#[derive(Resource, Debug, Default)]
pub struct SteamFeaturesDisabledNotice {
    pub reason: Option<SteamDisableReason>,
}

impl SteamWatchdog {
    pub fn state(&self) -> SteamState {
        self.state
    }

    pub fn pipe_disconnect_count(&self) -> u64 {
        u64::from(matches!(
            self.state,
            SteamState::Disabled {
                reason: SteamDisableReason::PipeDisconnect
            }
        ))
    }

    pub fn call<F, T>(&mut self, label: &'static str, f: F) -> Option<T>
    where
        F: FnOnce() -> Result<T, SteamError>,
    {
        self.call_result(label, f).ok()
    }

    /// Keep individual call failures distinct from a disabled Steam session.
    pub fn call_result<T>(
        &mut self,
        label: &'static str,
        f: impl FnOnce() -> Result<T, SteamError>,
    ) -> Result<T, SteamError> {
        if let SteamState::Disabled { reason } = self.state {
            return Err(SteamError::Disabled(reason));
        }
        f().inspect_err(|error| {
            if *error == SteamError::PipeDisconnect {
                self.disable(SteamDisableReason::PipeDisconnect);
            }
            warn!(target: "steam", label, %error, "Steam call failed");
        })
    }

    /// Let adapters with their own error signals explicitly disable Steam.
    pub fn disable(&mut self, reason: SteamDisableReason) {
        if self.state == SteamState::Active {
            self.state = SteamState::Disabled { reason };
        }
    }

    fn take_disable_notification(&mut self) -> Option<SteamDisableReason> {
        let SteamState::Disabled { reason } = self.state else {
            return None;
        };
        (!std::mem::replace(&mut self.notification_emitted, true)).then_some(reason)
    }
}

pub struct SteamWatchdogPlugin;

impl Plugin for SteamWatchdogPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SteamWatchdog>()
            .init_resource::<SteamFeaturesDisabledNotice>()
            .add_systems(Update, emit_steam_disabled_notice);
    }
}

pub fn emit_steam_disabled_notice(
    mut watchdog: ResMut<SteamWatchdog>,
    mut notice: ResMut<SteamFeaturesDisabledNotice>,
    mut commands: Commands,
) {
    if let Some(reason) = watchdog.take_disable_notification() {
        notice.reason = Some(reason);
        commands.trigger(SteamFeaturesDisabledEvent { reason });
        warn!(target: "steam", ?reason, "Steam features disabled until app restart");
    }
}

/// Adapter for APIs that expose pipe failures only through diagnostic text.
/// Prefer a typed mapping where one exists.
pub fn steam_error_from_debug(error: impl std::fmt::Debug) -> SteamError {
    let message = format!("{error:?}");
    let lower = message.to_ascii_lowercase();
    if lower.contains("broken pipe")
        || lower.contains("brokenpipe")
        || lower.contains("pipe disconnect")
        || lower.contains("pipedisconnect")
    {
        SteamError::PipeDisconnect
    } else {
        SteamError::CallFailed(message)
    }
}
