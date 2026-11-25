//! Windowed and headless application bootstrap helpers.

use std::time::Duration;

use bevy::{
    app::ScheduleRunnerPlugin,
    prelude::{MonitorSelection, UVec2, Vec2, Window, WindowPosition},
    window::{ExitCondition, WindowMode, WindowPlugin, WindowResizeConstraints, WindowResolution},
};

const DEFAULT_HEADLESS_TICK_RATE_HZ: f64 = 60.0;

/// Application-owned startup settings.
#[derive(Debug, Clone)]
pub struct WindowBootstrapConfig {
    pub title: String,
    /// Initial resolution at the default scale factor.
    pub size: UVec2,
    /// Minimum size in logical pixels.
    pub min_size: Vec2,
    pub resizable: bool,
    /// Use borderless fullscreen on the primary monitor.
    pub fullscreen: bool,
    pub position: WindowPosition,
    pub headless: bool,
    pub headless_tick_rate_hz: f64,
}

impl Default for WindowBootstrapConfig {
    fn default() -> Self {
        Self {
            title: Window::default().title,
            size: UVec2::new(1280, 720),
            min_size: Vec2::new(1.0, 1.0),
            resizable: true,
            fullscreen: false,
            position: WindowPosition::Automatic,
            headless: false,
            headless_tick_rate_hz: DEFAULT_HEADLESS_TICK_RATE_HZ,
        }
    }
}

/// Apply a force-fullscreen flag without clearing an existing preference.
pub fn apply_fullscreen_override(
    mut config: WindowBootstrapConfig,
    fullscreen: bool,
) -> WindowBootstrapConfig {
    config.fullscreen |= fullscreen;
    config
}

/// Build the descriptor independently of whether the app will create a window.
pub fn build_window_descriptor(config: &WindowBootstrapConfig) -> Window {
    Window {
        title: config.title.clone(),
        resolution: WindowResolution::new(config.size.x.max(1), config.size.y.max(1)),
        resize_constraints: WindowResizeConstraints {
            min_width: config.min_size.x.max(1.0),
            min_height: config.min_size.y.max(1.0),
            ..WindowResizeConstraints::default()
        },
        mode: if config.fullscreen {
            WindowMode::BorderlessFullscreen(MonitorSelection::Primary)
        } else {
            WindowMode::Windowed
        },
        resizable: config.resizable,
        position: config.position,
        ..Window::default()
    }
}

/// Configure Bevy's window plugin. A headless app spawns no primary window and
/// does not exit merely because there are no windows.
pub fn build_window_plugin(config: &WindowBootstrapConfig) -> WindowPlugin {
    if config.headless {
        WindowPlugin {
            primary_window: None,
            exit_condition: ExitCondition::DontExit,
            ..WindowPlugin::default()
        }
    } else {
        WindowPlugin {
            primary_window: Some(build_window_descriptor(config)),
            ..WindowPlugin::default()
        }
    }
}

/// Build a runner for a headless app. Invalid rates fall back to 60 Hz.
pub fn build_headless_schedule_runner(config: &WindowBootstrapConfig) -> ScheduleRunnerPlugin {
    let wait = Duration::try_from_secs_f64(config.headless_tick_rate_hz.recip())
        .ok()
        .filter(|wait| !wait.is_zero())
        .unwrap_or_else(|| Duration::from_secs_f64(DEFAULT_HEADLESS_TICK_RATE_HZ.recip()));
    ScheduleRunnerPlugin::run_loop(wait)
}
