use bevy::prelude::*;
use harbor_bevy_steam::{
    cloud::{CloudStorageBackend, CloudSyncError, InMemoryCloudBackend, load_typed, save_typed},
    watchdog::{
        SteamDisableReason, SteamError, SteamFeaturesDisabledEvent, SteamFeaturesDisabledNotice,
        SteamState, SteamWatchdog, SteamWatchdogPlugin, steam_error_from_debug,
    },
};

#[derive(Resource, Default)]
struct Notices(usize);

#[test]
fn watchdog_degrades_once_and_does_not_call_steam_again() {
    let mut app = App::new();
    app.add_plugins(SteamWatchdogPlugin)
        .init_resource::<Notices>()
        .add_observer(
            |_: On<SteamFeaturesDisabledEvent>, mut notices: ResMut<Notices>| {
                notices.0 += 1;
            },
        );

    let mut watchdog = app.world_mut().resource_mut::<SteamWatchdog>();
    assert_eq!(
        watchdog.call("temporary failure", || Err::<(), _>(
            SteamError::CallFailed("retry".into())
        )),
        None
    );
    assert_eq!(watchdog.call("retry", || Ok(42)), Some(42));
    assert_eq!(
        watchdog.call("disconnect", || Err::<(), _>(SteamError::PipeDisconnect)),
        None
    );
    assert_eq!(
        watchdog.call::<_, ()>("disabled", || panic!("called")),
        None
    );

    app.update();
    app.update();
    assert_eq!(app.world().resource::<Notices>().0, 1);
    assert_eq!(
        app.world().resource::<SteamFeaturesDisabledNotice>().reason,
        Some(SteamDisableReason::PipeDisconnect)
    );
    assert_eq!(
        app.world()
            .resource::<SteamWatchdog>()
            .pipe_disconnect_count(),
        1
    );
}

#[test]
fn watchdog_distinguishes_pipe_disconnects_from_ordinary_failures() {
    let mut watchdog = SteamWatchdog::default();
    assert_eq!(
        steam_error_from_debug(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
        SteamError::PipeDisconnect
    );
    assert!(matches!(
        steam_error_from_debug("remote write failed"),
        SteamError::CallFailed(_)
    ));
    watchdog.disable(SteamDisableReason::PipeDisconnect);
    watchdog.disable(SteamDisableReason::PipeDisconnect);
    assert_eq!(watchdog.pipe_disconnect_count(), 1);
    assert_eq!(
        watchdog.state(),
        SteamState::Disabled {
            reason: SteamDisableReason::PipeDisconnect
        }
    );
    assert_eq!(
        watchdog.call_result::<()>("disabled", || panic!("called")),
        Err(SteamError::Disabled(SteamDisableReason::PipeDisconnect))
    );
}

#[derive(Debug, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct Save {
    player: String,
    score: u64,
}

#[test]
fn cloud_round_trips_typed_data_by_filename_and_rejects_invalid_archives() {
    let mut backend = InMemoryCloudBackend::default();
    let save = Save {
        player: "Ada".into(),
        score: 42,
    };

    assert_eq!(load_typed::<Save>(&mut backend, "missing").unwrap(), None);
    save_typed(&mut backend, "slot-1", &save).unwrap();
    save_typed(&mut backend, "slot-2", &42_u64).unwrap();
    assert_eq!(
        load_typed::<Save>(&mut backend, "slot-1").unwrap(),
        Some(save)
    );
    assert_eq!(load_typed::<u64>(&mut backend, "slot-2").unwrap(), Some(42));

    for corrupt in [&[][..], &[1, 2, 3][..]] {
        backend.write("slot-1", corrupt).unwrap();
        assert!(matches!(
            load_typed::<Save>(&mut backend, "slot-1"),
            Err(CloudSyncError::Deserialize(_))
        ));
    }
}

#[test]
fn cloud_rejects_invalid_filenames_and_propagates_backend_failures() {
    struct Unavailable;
    impl CloudStorageBackend for Unavailable {
        fn read(&mut self, filename: &str) -> Result<Option<Vec<u8>>, CloudSyncError> {
            assert_eq!(filename, "valid");
            Err(std::io::Error::from(std::io::ErrorKind::NotConnected).into())
        }
        fn write(&mut self, filename: &str, _: &[u8]) -> Result<(), CloudSyncError> {
            assert_eq!(filename, "valid");
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied).into())
        }
    }
    let mut backend: Box<dyn CloudStorageBackend> = Box::new(Unavailable);
    assert!(matches!(
        load_typed::<u64>(&mut *backend, " "),
        Err(CloudSyncError::InvalidFilename)
    ));
    assert!(matches!(
        save_typed(&mut *backend, "invalid\0", &42_u64),
        Err(CloudSyncError::InvalidFilename)
    ));
    assert!(matches!(
        load_typed::<u64>(&mut *backend, "valid"),
        Err(CloudSyncError::Io(err)) if err.kind() == std::io::ErrorKind::NotConnected
    ));
    assert!(matches!(
        save_typed(&mut *backend, "valid", &42_u64),
        Err(CloudSyncError::Io(err)) if err.kind() == std::io::ErrorKind::PermissionDenied
    ));
}
