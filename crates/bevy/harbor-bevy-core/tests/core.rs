use std::{fs, time::Duration};

use bevy::{app::AppExit, prelude::*};
use harbor_bevy_core::{
    AppIdentity, FileStore, FileStoreError, PhaseReadinessTracker, PhaseTimer,
    change_notifier::{AppChangeNotifierExt, ComponentChanged, ResourceChanged},
    fatal::{
        FatalCode, GracefulShutdownPlugin, GracefulShutdownRequest, LastFatalExit, format_fatal,
    },
    phase::{ActionReady, PhaseReadiness},
    traits::Resettable,
};

#[derive(Debug, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct Record(String, u32);

#[test]
fn store_roundtrips_replaces_and_removes_typed_data() {
    let identity = AppIdentity::new("Example", "Sample");
    let expected = dirs::data_local_dir()
        .unwrap_or_else(|| ".".into())
        .join("Example/Sample");
    assert_eq!(identity.data_dir(), expected);
    assert_eq!(
        FileStore::new(&identity).root_dir(),
        Some(expected.as_path())
    );

    let scratch = Scratch::new();
    let path = scratch.0.join("nested");
    let store = FileStore::from_dir(&path);
    let first = Record("a longer string".into(), 42);
    let second = Record("short".into(), 17);
    store.set("key", &first).unwrap();
    assert_eq!(store.get::<Record>("key").unwrap(), first);
    store.set("key", &second).unwrap();
    assert_eq!(store.get::<Record>("key").unwrap(), second);
    assert_eq!(fs::read_dir(&path).unwrap().count(), 1);
    store.remove("key").unwrap();
    store.remove("key").unwrap();
    assert!(
        matches!(store.get::<Record>("key"), Err(FileStoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound)
    );
}

#[test]
fn store_rejects_invalid_keys_and_corrupt_archives_and_cleans_failed_writes() {
    let scratch = Scratch::new();
    let store = FileStore::from_dir(&scratch.0);
    for key in [
        "",
        ".",
        "..",
        "../outside",
        "dir/key",
        "dir\\key",
        "/absolute",
        "C:drive",
        "key\0",
    ] {
        assert!(matches!(
            store.set(key, &42_u32),
            Err(FileStoreError::InvalidKey(_))
        ));
        assert!(matches!(
            store.get::<u32>(key),
            Err(FileStoreError::InvalidKey(_))
        ));
        assert!(matches!(
            store.remove(key),
            Err(FileStoreError::InvalidKey(_))
        ));
    }
    fs::write(scratch.0.join("broken.bin"), []).unwrap();
    assert!(matches!(
        store.get::<Record>("broken"),
        Err(FileStoreError::Deserialize(_))
    ));
    fs::create_dir(scratch.0.join("blocked.bin")).unwrap();
    assert!(matches!(
        store.set("blocked", &42_u32),
        Err(FileStoreError::Io(_))
    ));
    assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 2);
}

#[test]
fn store_is_disabled_when_initialized_as_a_default_resource() {
    let mut app = App::new();
    app.init_resource::<FileStore>();
    let store = app.world().resource::<FileStore>();
    assert!(store.is_disabled());
    assert_eq!(store.root_dir(), None);
    store.set("key", &42_u32).unwrap();
    store.remove("key").unwrap();
    assert!(
        matches!(store.get::<u32>("key"), Err(FileStoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound)
    );
}

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("harbor-bevy-core-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Resource)]
struct Counter(u32);

#[derive(Component)]
struct Value(u32);

#[derive(Resource, Default)]
struct Changes(Vec<u32>, Vec<(Entity, u32)>);

#[test]
fn change_notifiers_handle_initial_insert_changes_and_duplicate_registration() {
    let mut app = App::new();
    app.init_resource::<Changes>()
        .observe_resource_change::<Counter>()
        .observe_component_change::<Value>()
        .observe_resource_change::<Counter>()
        .observe_component_change::<Value>()
        .add_observer(
            |_: On<ResourceChanged<Counter>>,
             source: Res<Counter>,
             mut changes: ResMut<Changes>| {
                changes.0.push(source.0);
            },
        )
        .add_observer(
            |event: On<ComponentChanged<Value>>,
             source: Query<&Value>,
             mut changes: ResMut<Changes>| {
                changes
                    .1
                    .push((event.entity, source.get(event.entity).unwrap().0));
            },
        );
    app.update(); // The resource need not exist when registering the notifier.
    app.insert_resource(Counter(1));
    let a = app.world_mut().spawn(Value(2)).id();
    let b = app.world_mut().spawn(Value(3)).id();
    app.update();
    app.world_mut().resource_mut::<Counter>().0 = 4;
    app.world_mut().get_mut::<Value>(a).unwrap().0 = 5;
    app.update();
    app.update();

    let changes = app.world().resource::<Changes>();
    assert_eq!(changes.0, [1, 4]);
    let mut components = changes.1.clone();
    components.sort_unstable_by_key(|entry| entry.1);
    assert_eq!(components, [(a, 2), (b, 3), (a, 5)]);
}

#[derive(Default)]
struct Ready(bool);
#[derive(Default)]
struct AlsoReady(bool);
harbor_bevy_core::impl_bool_phase_readiness!(Ready, AlsoReady,);

#[test]
fn phase_timer_and_readiness_work_as_ecs_data() {
    let mut timer = PhaseTimer::new(Duration::from_secs(2));
    timer.tick(Duration::from_millis(750));
    assert_eq!(timer.elapsed_secs(), 0.75);
    assert_eq!(timer.remaining_secs(), 1.25);
    assert!(!timer.is_finished());
    timer.tick(Duration::from_secs(3));
    assert!(timer.is_finished());
    assert_eq!(timer.elapsed_secs(), 2.0);
    assert_eq!(timer.remaining_secs(), 0.0);
    timer.reset();
    assert!(!timer.is_finished());
    assert_eq!(timer.elapsed_secs(), 0.0);
    assert_eq!(timer.remaining_secs(), 2.0);
    let mut immediate = PhaseTimer::default();
    immediate.tick(Duration::ZERO);
    assert!(immediate.is_finished());

    let mut ready = Ready::default();
    assert!(ready.is_action_ready());
    ready.mark_complete();
    ready.mark_complete();
    assert!(ready.is_complete());
    assert!(!ready.is_action_ready());
    ready.reset();
    assert!(ready.is_action_ready());
    assert!(!AlsoReady::default().is_complete());

    let mut tracker = PhaseReadinessTracker::new();
    assert!(!tracker.all_ready());
    assert!(!tracker.mark_ready(&"unknown"));
    assert!(tracker.register("a"));
    assert!(tracker.register("b"));
    assert!(tracker.mark_ready(&"a"));
    assert!(!tracker.register("a"));
    assert!(tracker.is_ready(&"a"));
    assert!(!tracker.mark_ready(&"a"));
    assert!(!tracker.is_ready(&"b"));
    assert_eq!(tracker.ready_count(), 1);
    assert_eq!(tracker.total_count(), 2);
    assert!(!tracker.all_ready());
    assert!(tracker.mark_ready(&"b"));
    assert!(tracker.all_ready());
    tracker.reset();
    let mut ids: Vec<_> = tracker.participants().copied().collect();
    ids.sort_unstable();
    assert_eq!(ids, ["a", "b"]);
    assert_eq!(tracker.ready_count(), 0);
    assert!(!tracker.all_ready());

    let mut app = App::new();
    app.init_resource::<PhaseReadinessTracker<&str>>();
    assert_eq!(
        app.world()
            .resource::<PhaseReadinessTracker<&str>>()
            .total_count(),
        0
    );
    let mut world = World::new();
    let entity = world.spawn((timer, tracker)).id();
    assert_eq!(
        world
            .get::<PhaseReadinessTracker<&str>>(entity)
            .unwrap()
            .total_count(),
        2
    );
    assert_eq!(
        world.get::<PhaseTimer>(entity).unwrap().remaining_secs(),
        2.0
    );
}

#[derive(Debug, thiserror::Error)]
#[error("example failure")]
struct Failure(u8);

impl FatalCode for Failure {
    fn exit_code(&self) -> u8 {
        self.0
    }

    fn label(&self) -> &str {
        "EXAMPLE"
    }
}

#[test]
fn shutdown_observer_writes_a_nonzero_exit_and_remembers_the_last_fatal() {
    assert_eq!(format_fatal(&Failure(7)), "[FATAL] example failure");
    for (input, expected) in [(7, 7), (0, 1)] {
        let mut app = App::new();
        app.add_plugins(GracefulShutdownPlugin);
        assert_eq!(
            *app.world().resource::<LastFatalExit>(),
            LastFatalExit::default()
        );
        assert_eq!(app.should_exit(), None);

        let request = GracefulShutdownRequest::new(Failure(input));
        assert_eq!(request.error.label(), "EXAMPLE");
        app.world_mut().trigger(request);
        let exit = std::num::NonZeroU8::new(expected).unwrap();
        assert_eq!(app.should_exit(), Some(AppExit::Error(exit)));
        assert_eq!(app.world().resource::<LastFatalExit>().0, Some(exit));
    }
}
