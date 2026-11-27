use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use bevy::prelude::App;
use harbor_bevy_platform::{DesktopBridgePlugin, HapticCue, Haptics, MockHaptics, PlatformBridges};
use monetization_policy::try_issue_ad_permit;

#[test]
fn desktop_plugin_mounts_inert_bridges_and_accepts_an_affine_permit() {
    struct Placement(Arc<AtomicBool>);

    impl Drop for Placement {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    let mut app = App::new();
    app.add_plugins(DesktopBridgePlugin::<Placement>::default());
    app.update();

    let bridges = app.world().resource::<PlatformBridges<Placement>>();
    bridges.haptics.play(HapticCue::Impact);
    assert_eq!(bridges.billing.purchase("sample.sku"), None);
    assert_eq!(bridges.push_tokens.token(), None);
    assert_eq!(bridges.sign_in.sign_in(), None);
    let dropped = Arc::new(AtomicBool::new(false));
    let permit = try_issue_ad_permit(Placement(Arc::clone(&dropped)), |_| Ok::<_, ()>(())).unwrap();
    assert!(!dropped.load(Ordering::Relaxed));
    assert!(!bridges.ads.show_ad(permit));
    assert!(dropped.load(Ordering::Relaxed));
}

#[test]
fn recording_mock_drains_haptics_in_order() {
    let mock = MockHaptics::default();
    mock.play(HapticCue::Tick);
    mock.play(HapticCue::Pulse(200));
    assert_eq!(mock.take_cues(), [HapticCue::Tick, HapticCue::Pulse(200)]);
    assert!(mock.take_cues().is_empty());
}

#[test]
fn desktop_plugin_preserves_an_installed_bridge() {
    struct CustomSignIn;

    impl harbor_bevy_platform::SignIn for CustomSignIn {
        fn sign_in(&self) -> Option<String> {
            Some("local-session".into())
        }
    }

    let mut app = App::new();
    app.insert_resource(PlatformBridges::<()> {
        sign_in: Box::new(CustomSignIn),
        ..Default::default()
    });
    app.add_plugins(DesktopBridgePlugin::<()>::default());
    let bridges = app.world().resource::<PlatformBridges<()>>();
    assert_eq!(bridges.sign_in.sign_in().as_deref(), Some("local-session"));
}
