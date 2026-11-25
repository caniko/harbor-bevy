use std::{
    fmt::{Debug, Display},
    num::NonZeroU8,
};

use bevy::{app::AppExit, prelude::*};

/// An application-defined fatal error with a process exit code and label.
pub trait FatalCode: Display + Debug + Send + Sync + 'static {
    fn exit_code(&self) -> u8;
    fn label(&self) -> &str;
}

/// Request that the app exit with a fatal error.
#[derive(Debug, Event)]
pub struct GracefulShutdownRequest {
    pub error: Box<dyn FatalCode>,
}

impl GracefulShutdownRequest {
    pub fn new(error: impl FatalCode) -> Self {
        Self {
            error: Box::new(error),
        }
    }
}

pub fn format_fatal(error: &dyn FatalCode) -> String {
    format!("[FATAL] {error}")
}

pub fn print_fatal(error: &dyn FatalCode) {
    eprintln!("{}", format_fatal(error));
}

/// The most recent error exit code emitted by the shutdown observer.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Resource)]
pub struct LastFatalExit(pub Option<NonZeroU8>);

pub struct GracefulShutdownPlugin;

impl Plugin for GracefulShutdownPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LastFatalExit>()
            .add_observer(on_graceful_shutdown);
    }
}

fn on_graceful_shutdown(
    request: On<GracefulShutdownRequest>,
    mut last_exit: ResMut<LastFatalExit>,
    mut exit: MessageWriter<AppExit>,
) {
    print_fatal(request.error.as_ref());
    let code = NonZeroU8::new(request.error.exit_code()).unwrap_or(NonZeroU8::MIN);
    last_exit.0 = Some(code);
    exit.write(AppExit::Error(code));
}
