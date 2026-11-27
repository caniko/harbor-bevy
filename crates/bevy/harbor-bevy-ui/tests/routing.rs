use bevy::prelude::*;
use harbor_bevy_ui::{
    focus_bounds::{ActionFocus, FocusBounds, FocusBoundsPort, advance_action_focus},
    pointer_router::{
        DisplayedHitTarget, LogicalRect, PointerId, PointerPhase, PointerRouter, PointerSample,
    },
    viewport::{CurrentViewport, SafeAreaInsets, ViewportPlugin},
};

const RECT: LogicalRect = LogicalRect {
    min: [0.0, 0.0],
    max: [10.0, 10.0],
};

#[test]
fn viewport_preserves_insets_and_clamps_usable_area() {
    let insets = SafeAreaInsets {
        top: 10.0,
        right: 20.0,
        bottom: 30.0,
        left: 40.0,
    };
    let mut app = App::new();
    app.insert_resource(insets).add_plugins(ViewportPlugin);
    assert_eq!(*app.world().resource::<SafeAreaInsets>(), insets);
    let metrics = &app.world().resource::<CurrentViewport>().0;
    assert_eq!(
        metrics.usable_rect(insets),
        LogicalRect {
            min: [40.0, 10.0],
            max: [1260.0, 690.0]
        }
    );
    let rect = SafeAreaInsets {
        top: f32::INFINITY,
        right: f32::MAX,
        bottom: -1.0,
        left: f32::NAN,
    }
    .usable_rect(Vec2::new(100.0, 200.0));
    assert_eq!(
        rect,
        LogicalRect {
            min: [0.0, 200.0],
            max: [0.0, 200.0]
        }
    );
    assert_eq!(rect.size(), [0.0, 0.0]);
    assert_eq!(
        insets.usable_rect(Vec2::new(f32::NAN, -1.0)).size(),
        [0.0, 0.0]
    );
}

#[test]
fn press_uses_topmost_displayed_target_and_owns_gesture() {
    use PointerPhase::{Move, Press, Release};
    let mut router = PointerRouter::default();
    let targets = [
        DisplayedHitTarget {
            target: 1,
            bounds: RECT,
            z_order: 2,
        },
        DisplayedHitTarget {
            target: 2,
            bounds: RECT,
            z_order: 2,
        },
        DisplayedHitTarget {
            target: 3,
            bounds: RECT,
            z_order: 1,
        },
    ];
    assert!(router.trace().is_empty());
    let mut route = |id, phase, position, targets| {
        router
            .route(
                PointerSample {
                    id: PointerId(id),
                    phase,
                    position,
                },
                targets,
            )
            .target
    };
    assert_eq!(route(0, Press, [1.0, 1.0], &targets), Some(2));
    assert_eq!(route(1, Press, [10.0, 1.0], &targets), None);
    assert_eq!(route(1, Move, [1.0, 1.0], &targets), None);
    assert_eq!(route(0, Move, [99.0, 99.0], &[]), Some(2));
    assert_eq!(route(0, Release, [99.0, 99.0], &[]), Some(2));
    assert_eq!(route(0, Release, [1.0, 1.0], &targets), None);
    assert!(router.trace().is_empty());
}

#[test]
fn cancellation_releases_only_that_pointer_and_trace_evicts_oldest() {
    use PointerPhase::{Cancel, Move, Press};
    let mut router = PointerRouter::default();
    router.set_trace_capacity(2);
    let targets = [DisplayedHitTarget {
        target: "item",
        bounds: RECT,
        z_order: 0,
    }];
    let mut route = |id, phase| {
        router.route(
            PointerSample {
                id: PointerId(id),
                phase,
                position: [1.0, 1.0],
            },
            &targets,
        )
    };
    route(0, Press);
    route(1, Press);
    assert_eq!(route(0, Cancel).target, Some("item"));
    let uncaptured = route(0, Move);
    assert_eq!(uncaptured.target, None);
    let captured = route(1, Move);
    assert_eq!(captured.target, Some("item"));
    assert!(router.trace().iter().eq([&uncaptured, &captured]));
    router.set_trace_capacity(0);
    assert!(router.trace().is_empty());
}

#[test]
fn tab_navigation_wraps_skips_unfocusable_and_recovers_removed_target() {
    let mut app = App::new();
    app.insert_resource(FocusBoundsPort {
        targets: [(1, true), (2, false), (3, true)]
            .map(|(target, keyboard_focusable)| FocusBounds {
                target,
                bounds: RECT,
                keyboard_focusable,
            })
            .into(),
    })
    .init_resource::<ActionFocus<i32>>()
    .init_resource::<ButtonInput<KeyCode>>()
    .add_systems(Update, advance_action_focus::<i32>);

    let tab = |app: &mut App, reverse| {
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.reset_all();
        keys.press(KeyCode::Tab);
        if reverse {
            keys.press(KeyCode::ShiftRight);
        }
        app.update();
        app.world().resource::<ActionFocus<i32>>().0
    };
    assert_eq!(tab(&mut app, false), Some(1));
    assert_eq!(tab(&mut app, false), Some(3));
    assert_eq!(tab(&mut app, false), Some(1));
    assert_eq!(tab(&mut app, true), Some(3));
    assert_eq!(tab(&mut app, true), Some(1));
    app.world_mut()
        .resource_mut::<FocusBoundsPort<i32>>()
        .targets
        .remove(0);
    app.world_mut()
        .resource_mut::<ButtonInput<KeyCode>>()
        .reset_all();
    app.update();
    assert_eq!(app.world().resource::<ActionFocus<i32>>().0, None);
    assert_eq!(tab(&mut app, true), Some(3));
    app.world_mut()
        .resource_mut::<FocusBoundsPort<i32>>()
        .targets
        .clear();
    assert_eq!(tab(&mut app, false), None);
}

#[cfg(any(feature = "serde", feature = "rkyv"))]
type UiSnapshot = (
    SafeAreaInsets,
    FocusBoundsPort<u32>,
    ActionFocus<u32>,
    DisplayedHitTarget<u32>,
    harbor_bevy_ui::pointer_router::PointerRouteDecision<u32>,
);

#[cfg(any(feature = "serde", feature = "rkyv"))]
fn snapshot() -> UiSnapshot {
    let hit = DisplayedHitTarget {
        target: 7,
        bounds: RECT,
        z_order: 0,
    };
    let decision = PointerRouter::default().route(
        PointerSample {
            id: PointerId(1),
            phase: PointerPhase::Press,
            position: RECT.min,
        },
        &[hit],
    );
    (
        SafeAreaInsets {
            bottom: 12.0,
            ..Default::default()
        },
        FocusBoundsPort {
            targets: vec![FocusBounds {
                target: hit.target,
                bounds: hit.bounds,
                keyboard_focusable: true,
            }],
        },
        ActionFocus(Some(hit.target)),
        hit,
        decision,
    )
}

#[cfg(feature = "serde")]
#[test]
fn routing_snapshot_round_trips_through_serde() {
    let snapshot = snapshot();
    let json = serde_json::to_string(&snapshot).unwrap();
    assert_eq!(serde_json::from_str::<UiSnapshot>(&json).unwrap(), snapshot);
}

#[cfg(feature = "rkyv")]
#[test]
fn routing_snapshot_round_trips_through_rkyv() {
    let snapshot = snapshot();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&snapshot).unwrap();
    assert_eq!(
        rkyv::from_bytes::<UiSnapshot, rkyv::rancor::Error>(&bytes).unwrap(),
        snapshot
    );
}
