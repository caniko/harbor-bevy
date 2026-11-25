//! Manually drive Bevy's real schedules without a window or event loop.

use std::{sync::Once, time::Duration};

use bevy::{
    app::{AppExit, PluginsState, TaskPoolPlugin},
    ecs::{component::Mutable, message::MessageId, observer::IntoObserver},
    log::LogPlugin,
    prelude::*,
    state::{app::StatesPlugin, state::FreelyMutableState},
    time::{Fixed, Real, TimePlugin, TimeUpdateStrategy, Virtual},
};

/// Setup settings. The world receives a copy for systems to read; replacing
/// that copy does not reconfigure the harness.
#[derive(Clone, Debug, Resource)]
pub struct HeadlessConfig {
    /// Seed for the application to use; the harness does not install an RNG.
    pub seed: u64,
    /// Nonzero duration of a fixed tick and the default delta per frame.
    pub fixed_dt: Duration,
    /// Total number of updates allowed, including the first startup frame.
    pub frame_budget: Option<u64>,
    /// Attempt to install Bevy's process-global logger once. This does not
    /// silence an existing subscriber when `false`.
    pub enable_logging: bool,
}

impl Default for HeadlessConfig {
    fn default() -> Self {
        Self {
            seed: 0,
            fixed_dt: Time::<Fixed>::default().timestep(),
            frame_budget: None,
            enable_logging: false,
        }
    }
}

/// Application-specific setup, action dispatch, and terminal outcome.
pub trait HeadlessGame {
    type Action;
    type Outcome;

    fn build(app: &mut App, cfg: &HeadlessConfig);
    fn apply(app: &mut HeadlessApp, action: Self::Action) -> anyhow::Result<()>;
    fn outcome(world: &World) -> Option<Self::Outcome>;
}

/// A Bevy app with explicit time, a frame counter, and a latched exit status.
pub struct HeadlessApp {
    app: App,
    config: HeadlessConfig,
    frames: u64,
    exit: Option<AppExit>,
}

impl HeadlessApp {
    /// Create an app with task pools, time and state support. No updates run
    /// during construction. Panics if `config.fixed_dt` is zero.
    pub fn new(config: HeadlessConfig) -> Self {
        let mut app = App::new();
        app.add_plugins((TaskPoolPlugin::default(), TimePlugin, StatesPlugin))
            .insert_resource(config.clone())
            .insert_resource(Time::<Fixed>::from_duration(config.fixed_dt))
            .insert_resource(TimeUpdateStrategy::ManualDuration(config.fixed_dt));
        if config.enable_logging {
            static LOGGING: Once = Once::new();
            LOGGING.call_once(|| {
                app.add_plugins(LogPlugin::default());
            });
        }

        // Bevy's first real-clock sample establishes a baseline but does not
        // advance time. Set it before startup so frame zero gets a full delta.
        app.world_mut()
            .resource_mut::<Time<Real>>()
            .update_with_duration(Duration::ZERO);
        app.world_mut()
            .resource_mut::<Time<Virtual>>()
            .set_max_delta(Duration::MAX);

        Self {
            app,
            config,
            frames: 0,
            exit: None,
        }
    }

    /// Run the adapter's setup without stepping the app.
    pub fn for_game<G: HeadlessGame>(config: HeadlessConfig) -> Self {
        let mut headless = Self::new(config);
        G::build(&mut headless.app, &headless.config);
        headless
    }

    /// Run one frame with the configured delta, unless budget or exit stopped it.
    pub fn update(&mut self) -> Option<AppExit> {
        self.update_with_delta(self.config.fixed_dt)
    }

    /// Run one frame with a caller-supplied delta (including zero). Bevy
    /// accumulates fractional fixed ticks and respects virtual pause and speed.
    ///
    /// Plugins must be ready synchronously on the first step. If setup is
    /// asynchronous, wait for readiness through [`Self::app_mut`] first.
    pub fn update_with_delta(&mut self, delta: Duration) -> Option<AppExit> {
        self.exit = self.app_exit();
        if !self.can_step() {
            return self.exit.clone();
        }
        self.finish_plugins();
        self.insert_resource(TimeUpdateStrategy::ManualDuration(delta));
        crate::fault_injector::with_frame_scope(self.world().id(), || self.app.update());
        self.frames = self.frames.saturating_add(1);
        self.exit = self.app.should_exit();
        self.exit.clone()
    }

    pub(crate) fn finish_plugins(&mut self) {
        match self.app.plugins_state() {
            PluginsState::Ready => {
                self.app.finish();
                self.app.cleanup();
            }
            PluginsState::Finished => self.app.cleanup(),
            PluginsState::Cleaned => {}
            PluginsState::Adding => panic!("headless app plugins are not ready"),
        }
    }

    /// Run up to `count` frames and return the number actually executed.
    pub fn run_frames(&mut self, count: u64) -> u64 {
        let start = self.frames;
        for _ in 0..count {
            if !self.can_step() {
                break;
            }
            self.update();
        }
        self.frames - start
    }

    /// Run one full frame per fixed-time quantum, returning the frames run.
    /// At normal virtual speed this advances one fixed tick per frame. To
    /// test multiple ticks in a single frame, use [`Self::update_with_delta`].
    pub fn advance_fixed(&mut self, steps: u64) -> u64 {
        let start = self.frames;
        for _ in 0..steps {
            if !self.can_step() {
                break;
            }
            let timestep = self.world().resource::<Time<Fixed>>().timestep();
            self.update_with_delta(timestep);
        }
        self.frames - start
    }

    /// Check before the first update and after every update, never stepping
    /// more than `max_frames` or beyond the app's own frame budget.
    pub fn run_until(&mut self, max_frames: u64, mut pred: impl FnMut(&World) -> bool) -> bool {
        for _ in 0..max_frames {
            if pred(self.world()) {
                return true;
            }
            if !self.can_step() {
                return false;
            }
            self.update();
        }
        pred(self.world())
    }

    /// Apply an action without updating, useful for several same-frame actions.
    pub fn apply_action<G: HeadlessGame>(&mut self, action: G::Action) -> anyhow::Result<()> {
        anyhow::ensure!(self.can_step(), "headless app cannot accept more actions");
        G::apply(self, action)
    }

    /// Apply an action, step once if the app is still running, then poll the game.
    pub fn step_action<G: HeadlessGame>(
        &mut self,
        action: G::Action,
    ) -> anyhow::Result<Option<G::Outcome>> {
        self.apply_action::<G>(action)?;
        self.update();
        Ok(self.outcome::<G>())
    }

    pub fn outcome<G: HeadlessGame>(&self) -> Option<G::Outcome> {
        G::outcome(self.world())
    }

    /// Number of completed updates (the next frame's zero-based index).
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Exit emitted by the app, including one posted between updates.
    pub fn app_exit(&self) -> Option<AppExit> {
        self.exit.clone().or_else(|| self.app.should_exit())
    }

    pub fn can_step(&self) -> bool {
        self.config.frame_budget.is_none_or(|cap| self.frames < cap) && self.app_exit().is_none()
    }

    pub fn app(&self) -> &App {
        &self.app
    }

    /// Set up plugins or systems. Step through the harness rather than calling
    /// `App::update` directly so accounting and exit detection stay in sync.
    pub fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }

    pub fn world(&self) -> &World {
        self.app.world()
    }

    pub fn world_mut(&mut self) -> &mut World {
        self.app.world_mut()
    }

    pub fn insert_resource<R: Resource>(&mut self, resource: R) {
        self.world_mut().insert_resource(resource);
    }

    pub fn resource<R: Resource>(&self) -> Option<&R> {
        self.world().get_resource()
    }

    pub fn resource_mut<R: Resource<Mutability = Mutable>>(&mut self) -> Option<Mut<'_, R>> {
        self.world_mut().get_resource_mut()
    }

    pub fn state<S: States>(&self) -> Option<&S> {
        self.resource::<State<S>>().map(State::get)
    }

    /// Queue a transition on an already registered state.
    pub fn set_next_state<S: FreelyMutableState>(&mut self, next: S) {
        self.world_mut().resource_mut::<NextState<S>>().set(next);
    }

    /// Enqueue a buffered Bevy message, or return `None` if unregistered.
    pub fn write_message<M: Message>(&mut self, message: M) -> Option<MessageId<M>> {
        self.world_mut().write_message(message)
    }

    /// Trigger an event and its observers immediately.
    pub fn trigger<E: Event>(&mut self, event: E)
    where
        for<'a> E::Trigger<'a>: Default,
    {
        self.world_mut().trigger(event);
    }

    pub fn add_observer<M>(&mut self, observer: impl IntoObserver<M>) {
        self.app.add_observer(observer);
    }
}

impl Default for HeadlessApp {
    fn default() -> Self {
        Self::new(HeadlessConfig::default())
    }
}
