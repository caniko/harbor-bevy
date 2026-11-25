//! Opt-in, per-world fault rules for instrumented test operations.

use std::{borrow::Borrow, cell::RefCell, collections::BTreeMap, fmt, sync::Arc};

use bevy::{ecs::world::WorldId, prelude::*};
use serde::{Deserialize, Serialize};

/// A caller-defined operation name, such as `settings.read`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OpKey(pub String);

impl Borrow<str> for OpKey {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// A probe returns the action for the caller to interpret, except for an
/// explicitly enabled live panic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultAction {
    Fail(String),
    Panic(String),
}

impl fmt::Display for FaultAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fail(message) => write!(f, "injected failure: {message}"),
            Self::Panic(message) => write!(f, "injected panic: {message}"),
        }
    }
}

impl std::error::Error for FaultAction {}

/// One reproducible fault rule, active on every matching probe in its frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaultInjectionEnvelope {
    /// Zero-based frame since the injector plugin was installed.
    pub frame: u64,
    pub operation: OpKey,
    pub action: FaultAction,
}

impl FaultInjectionEnvelope {
    pub fn new(frame: u64, operation: impl Into<String>, action: FaultAction) -> Self {
        Self {
            frame,
            operation: OpKey(operation.into()),
            action,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct FrameFaults {
    rules: Option<Arc<BTreeMap<OpKey, FaultAction>>>,
    panic_as_data: bool,
}

impl FrameFaults {
    fn check(&self, operation: &str) -> Option<FaultAction> {
        self.rules.as_ref()?.get(operation).cloned()
    }
}

/// World-local schedule and active snapshot. Use [`Self::probe`] from ECS
/// systems, including those running on a worker thread.
#[derive(Debug, Resource)]
pub struct FaultInjector {
    scheduled: BTreeMap<u64, BTreeMap<OpKey, FaultAction>>,
    frame: Option<u64>,
    active: FrameFaults,
    panic_as_data: bool,
}

impl Default for FaultInjector {
    fn default() -> Self {
        Self {
            scheduled: BTreeMap::new(),
            frame: None,
            active: FrameFaults::default(),
            panic_as_data: true,
        }
    }
}

impl FaultInjector {
    /// Add a future rule, returning the rule it replaced for the same frame
    /// and operation. A frame can no longer be scheduled after it syncs.
    pub fn schedule(
        &mut self,
        rule: FaultInjectionEnvelope,
    ) -> anyhow::Result<Option<FaultAction>> {
        anyhow::ensure!(
            self.frame.is_none_or(|frame| frame < rule.frame),
            "fault frame {} has already started",
            rule.frame
        );
        Ok(self
            .scheduled
            .entry(rule.frame)
            .or_default()
            .insert(rule.operation, rule.action))
    }

    /// The currently active frame; `None` before the first `First` schedule.
    pub fn frame(&self) -> Option<u64> {
        self.frame
    }

    /// From the next sync, return planned panics as `Err(FaultAction::Panic)`
    /// rather than unwinding. This is enabled by default.
    pub fn set_panic_as_data(&mut self, enabled: bool) {
        self.panic_as_data = enabled;
    }

    pub fn probe(&self, operation: &str) -> Result<(), FaultAction> {
        apply(self.active.check(operation), self.active.panic_as_data)
    }

    /// Expose the current world's snapshot to plain functions on this thread.
    /// Restores the previous scope even if `f` unwinds.
    pub fn scope<T>(&self, f: impl FnOnce() -> T) -> T {
        with_scope(None, self.active.clone(), f)
    }
}

/// Install the injector and synchronize it once in each `First` schedule.
pub struct FaultInjectorPlugin;

/// Order any consumers in `First` after this set.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FaultSync;

impl Plugin for FaultInjectorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FaultInjector>()
            .add_systems(First, sync_frame.in_set(FaultSync));
    }
}

// Exclusive systems run on the schedule thread, not an executor worker.
fn sync_frame(world: &mut World) {
    let world_id = world.id();
    let mut injector = world.resource_mut::<FaultInjector>();
    let frame = injector.frame.map_or(0, |frame| frame.saturating_add(1));
    injector.frame = Some(frame);
    injector.active = FrameFaults {
        rules: injector.scheduled.remove(&frame).map(Arc::new),
        panic_as_data: injector.panic_as_data,
    };
    FAULT_SCOPES.with_borrow_mut(|stack| {
        if let Some((scope_world, active)) = stack.last_mut()
            && *scope_world == Some(world_id)
        {
            *active = injector.active.clone();
        }
    });
}

thread_local! {
    static FAULT_SCOPES: RefCell<Vec<(Option<WorldId>, FrameFaults)>> = const { RefCell::new(Vec::new()) };
}

/// Probe plain code on the calling thread. Inside a [`crate::HeadlessApp`]
/// update this sees the active frame after [`FaultSync`]. Outside it, use
/// [`FaultInjector::scope`] or a direct world probe. Other threads do not
/// inherit the scope.
pub fn probe_fault(operation: &str) -> Result<(), FaultAction> {
    let (action, panic_as_data) = FAULT_SCOPES.with_borrow(|stack| {
        stack.last().map_or((None, true), |(_, active)| {
            (active.check(operation), active.panic_as_data)
        })
    });
    apply(action, panic_as_data)
}

fn apply(action: Option<FaultAction>, panic_as_data: bool) -> Result<(), FaultAction> {
    if let Some(FaultAction::Panic(message)) = &action
        && !panic_as_data
    {
        panic!("injected panic: {message}");
    }
    action.map_or(Ok(()), Err)
}

pub(crate) fn with_frame_scope<T>(world: WorldId, f: impl FnOnce() -> T) -> T {
    with_scope(Some(world), FrameFaults::default(), f)
}

fn with_scope<T>(world: Option<WorldId>, active: FrameFaults, f: impl FnOnce() -> T) -> T {
    struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            FAULT_SCOPES.with_borrow_mut(|stack| {
                stack.pop();
            });
        }
    }

    FAULT_SCOPES.with_borrow_mut(|stack| stack.push((world, active)));
    let _guard = Guard;
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_restore_across_nested_updates_and_opt_in_panic() {
        let mut app = App::new();
        app.add_plugins(FaultInjectorPlugin);
        let mut injector = app.world_mut().resource_mut::<FaultInjector>();
        injector.set_panic_as_data(false);
        injector
            .schedule(FaultInjectionEnvelope::new(
                0,
                "test",
                FaultAction::Panic("boom".into()),
            ))
            .unwrap();
        app.update();

        let injector = app.world().resource::<FaultInjector>();
        assert_eq!(injector.frame(), Some(0));
        let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            injector.scope(|| {
                crate::HeadlessApp::default().update();
                probe_fault("test").unwrap();
            });
        }))
        .unwrap_err();
        assert_eq!(
            payload.downcast_ref::<String>().unwrap(),
            "injected panic: boom"
        );
        assert_eq!(probe_fault("test"), Ok(()));
        assert!(
            app.world_mut()
                .resource_mut::<FaultInjector>()
                .schedule(FaultInjectionEnvelope::new(
                    0,
                    "test",
                    FaultAction::Fail("late".into())
                ))
                .is_err()
        );
    }
}
