use std::{borrow::Cow, time::Duration};

use bevy::{
    camera::{RenderTarget, Viewport},
    input::{
        mouse::{MouseScrollPixelsPerLine, MouseScrollUnit},
        touch::TouchPhase,
    },
    picking::{
        backend::HitData,
        events::{Pointer, PointerScroll},
        pointer::{Location, PointerId},
    },
    prelude::*,
    text::{DEFAULT_REM_SIZE_PX, FontSize},
    time::Real,
    ui::{ComputedNode, Overflow, ScrollPosition},
    window::{ExitCondition, PrimaryWindow, WindowMode, WindowResized, WindowResolution},
};
use harbor_bevy_ui::{
    action_id::{UiActionId, UiActionLabel},
    reactive::{LayoutReactive, LayoutReactivePlugin, RelayoutTriggered},
    scroll::apply_scroll,
    toast::{ToastPlugin, ToastTimer},
    viewport::{
        CurrentViewport, DisplaySettings, LayoutDensity, LayoutVersion, LetterboxState,
        PendingViewportResize, UiScaleChoice, ViewportConfig, ViewportMetrics, ViewportPlugin,
        ViewportSettleOverride,
    },
    window::{
        WindowBootstrapConfig, apply_fullscreen_override, build_headless_schedule_runner,
        build_window_descriptor, build_window_plugin,
    },
};

fn viewport_app(config: ViewportConfig, size: UVec2) -> (App, Entity) {
    let mut app = App::new();
    app.insert_resource(Time::<Real>::default())
        .insert_resource(config)
        .add_plugins(ViewportPlugin);
    let window = app
        .world_mut()
        .spawn((
            PrimaryWindow,
            Window {
                resolution: WindowResolution::new(size.x, size.y),
                ..default()
            },
        ))
        .id();
    (app, window)
}

fn step(app: &mut App, millis: u64) {
    app.world_mut()
        .resource_mut::<Time<Real>>()
        .advance_by(Duration::from_millis(millis));
    app.update();
}

fn resize(app: &mut App, window: Entity, width: u32, height: u32, message: bool) {
    app.world_mut()
        .get_mut::<Window>(window)
        .unwrap()
        .resolution
        .set_physical_resolution(width, height);
    if message {
        notify_resize(app, window, width as f32, height as f32);
    }
}

fn notify_resize(app: &mut App, window: Entity, width: f32, height: f32) {
    app.world_mut().write_message(WindowResized {
        window,
        width,
        height,
    });
}

fn metrics(app: &App) -> ViewportMetrics {
    app.world().resource::<CurrentViewport>().0
}

fn version(app: &App) -> u64 {
    app.world().resource::<LayoutVersion>().0
}

#[test]
fn resize_commits_on_startup_then_settles_from_last_distinct_sample() {
    let (mut app, window) = viewport_app(default(), UVec2::new(1280, 720));
    step(&mut app, 0);
    assert_eq!(version(&app), 1);
    assert_eq!(metrics(&app).logical_size, Vec2::new(1280.0, 720.0));
    resize(&mut app, window, 1400, 800, true);
    step(&mut app, 50);
    resize(&mut app, window, 1600, 900, false);
    step(&mut app, 100);
    notify_resize(&mut app, window, 1600.0, 900.0);
    step(&mut app, 119);
    assert_eq!(version(&app), 1);
    assert!(
        app.world()
            .resource::<PendingViewportResize>()
            .has_pending()
    );
    step(&mut app, 1);
    assert_eq!(version(&app), 2);
    assert_eq!(metrics(&app).physical_size, UVec2::new(1600, 900));
    step(&mut app, 200);
    assert_eq!(version(&app), 2);
}

#[test]
fn stale_messages_minimization_and_return_to_committed_size_do_not_publish() {
    let (mut app, window) = viewport_app(default(), UVec2::new(1280, 720));
    step(&mut app, 0);
    let secondary = app.world_mut().spawn(Window::default()).id();
    notify_resize(&mut app, secondary, 3000.0, 2000.0);
    notify_resize(&mut app, window, 1400.0, 800.0);
    step(&mut app, 120);
    assert!(
        !app.world()
            .resource::<PendingViewportResize>()
            .has_pending()
    );
    resize(&mut app, window, 1400, 800, true);
    step(&mut app, 0);
    resize(&mut app, window, 1280, 720, false);
    step(&mut app, 200);
    assert!(
        !app.world()
            .resource::<PendingViewportResize>()
            .has_pending()
    );
    resize(&mut app, window, 0, 0, true);
    step(&mut app, 200);
    assert_eq!(version(&app), 1);
    assert_eq!(metrics(&app).physical_size, UVec2::new(1280, 720));
}

#[test]
fn a_confirmed_resize_burst_restarts_the_timer_even_if_the_live_size_returns() {
    let (mut app, window) = viewport_app(default(), UVec2::new(1280, 720));
    step(&mut app, 0);
    resize(&mut app, window, 1600, 900, true);
    step(&mut app, 0);
    notify_resize(&mut app, window, 1500.0, 850.0);
    notify_resize(&mut app, window, 1600.0, 900.0);
    step(&mut app, 120);
    assert_eq!(version(&app), 1);
    step(&mut app, 120);
    assert_eq!(version(&app), 2);
}

#[test]
fn dpi_changes_publish_without_changing_logical_density_or_user_scale() {
    let (mut app, window) = viewport_app(default(), UVec2::new(1920, 1080));
    step(&mut app, 0);
    let original = metrics(&app);
    assert_eq!(original.density, LayoutDensity::Standard);
    {
        let mut window = app.world_mut().get_mut::<Window>(window).unwrap();
        window.resolution.set_scale_factor_override(Some(2.0));
        window.resolution.set_physical_resolution(3840, 2160);
    }
    step(&mut app, 0);
    let hidpi = metrics(&app);
    assert_eq!(version(&app), 2);
    assert_eq!(hidpi.scale_factor, 2.0);
    assert_eq!(hidpi.logical_size, original.logical_size);
    assert_eq!(hidpi.density, original.density);
    assert_eq!(hidpi.user_scale, original.user_scale);
    assert_eq!(hidpi.effective_scale, original.effective_scale);
    app.world_mut().resource_mut::<DisplaySettings>().ui_scale = UiScaleChoice::Scale125;
    step(&mut app, 0);
    assert_eq!(version(&app), 3);
    assert_eq!(metrics(&app).effective_scale, 1.875);
    assert!(
        matches!(metrics(&app).font_size_rem(DEFAULT_REM_SIZE_PX), FontSize::Rem(value) if value == 1.875)
    );
}

#[test]
fn tolerance_and_zero_settle_override_are_configurable() {
    let (mut app, window) = viewport_app(
        ViewportConfig {
            resize_tolerance_px: 2.0,
            ..default()
        },
        UVec2::new(1280, 720),
    );
    app.insert_resource(ViewportSettleOverride(Some(Duration::ZERO)));
    step(&mut app, 0);
    resize(&mut app, window, 1281, 720, true);
    step(&mut app, 0);
    assert_eq!(version(&app), 1);
    resize(&mut app, window, 1282, 720, false);
    step(&mut app, 0);
    assert_eq!(version(&app), 2);
}

#[test]
fn density_thresholds_use_both_logical_axes_and_custom_scale_is_validated() {
    let config = ViewportConfig {
        compact_max_size: Vec2::new(800.0, 500.0),
        spacious_min_size: Vec2::new(1600.0, 900.0),
        ..default()
    };
    let density = |width, height| {
        ViewportMetrics::from_physical_size(UVec2::new(width, height), 1.0, &config, &default())
            .unwrap()
            .density
    };
    assert_eq!(density(1800, 500), LayoutDensity::Compact);
    assert_eq!(density(1600, 899), LayoutDensity::Standard);
    assert_eq!(density(1600, 900), LayoutDensity::Spacious);
    for (scale, expected) in [
        (UiScaleChoice::Scale75, 0.75),
        (UiScaleChoice::Scale100, 1.0),
        (UiScaleChoice::Scale125, 1.25),
        (UiScaleChoice::Scale150, 1.5),
        (UiScaleChoice::Custom(1.75), 1.75),
        (UiScaleChoice::Custom(f32::NAN), 1.0),
        (UiScaleChoice::Custom(-1.0), 1.0),
    ] {
        assert_eq!(scale.multiplier(), expected);
    }
}

fn ortho_scale(app: &App, camera: Entity) -> f32 {
    let Projection::Orthographic(ortho) = app.world().get::<Projection>(camera).unwrap() else {
        panic!("expected orthographic projection");
    };
    ortho.scale
}

#[test]
fn letterboxing_centers_and_scales_only_primary_cameras_then_restores_them() {
    let (mut app, window) = viewport_app(
        ViewportConfig {
            enable_letterboxing: true,
            ..default()
        },
        UVec2::new(3840, 1800),
    );
    app.world_mut()
        .get_mut::<Window>(window)
        .unwrap()
        .resolution
        .set_scale_factor_override(Some(2.0));
    let camera = app.world_mut().spawn(Camera2d).id();
    let offscreen = app
        .world_mut()
        .spawn((
            Camera2d,
            RenderTarget::None {
                size: UVec2::splat(256),
            },
            Camera {
                viewport: Some(Viewport::default()),
                ..default()
            },
        ))
        .id();
    step(&mut app, 0);
    let viewport = app
        .world()
        .get::<Camera>(camera)
        .unwrap()
        .viewport
        .as_ref()
        .unwrap();
    assert_eq!(viewport.physical_position, UVec2::new(320, 0));
    assert_eq!(viewport.physical_size, UVec2::new(3200, 1800));
    assert_eq!(metrics(&app).logical_size, Vec2::new(1600.0, 900.0));
    assert_eq!(ortho_scale(&app, camera), 0.8);
    assert_eq!(ortho_scale(&app, offscreen), 1.0);
    assert_eq!(
        app.world()
            .get::<Camera>(offscreen)
            .unwrap()
            .viewport
            .as_ref()
            .unwrap()
            .physical_size,
        UVec2::ONE,
    );
    let late = app
        .world_mut()
        .spawn((
            Camera2d,
            Projection::Orthographic(OrthographicProjection {
                scale: 2.0,
                ..OrthographicProjection::default_2d()
            }),
            Camera {
                viewport: Some(Viewport {
                    physical_size: UVec2::splat(256),
                    depth: 0.1..0.9,
                    ..default()
                }),
                ..default()
            },
        ))
        .id();
    step(&mut app, 0);
    assert_eq!(ortho_scale(&app, late), 0.8);
    app.world_mut()
        .resource_mut::<ViewportConfig>()
        .reference_size = Vec2::splat(900.0);
    step(&mut app, 0);
    assert_eq!(ortho_scale(&app, late), 1.0);
    resize(&mut app, window, 1800, 1800, false);
    step(&mut app, 0);
    let viewport = app
        .world()
        .get::<Camera>(camera)
        .unwrap()
        .viewport
        .as_ref()
        .unwrap();
    assert_eq!(viewport.physical_size, UVec2::new(1800, 1013));
    assert_eq!(viewport.physical_position, UVec2::new(0, 393));
    app.world_mut()
        .resource_mut::<ViewportConfig>()
        .enable_letterboxing = false;
    step(&mut app, 0);
    assert!(app.world().resource::<LetterboxState>().viewport.is_none());
    assert!(
        app.world()
            .get::<Camera>(camera)
            .unwrap()
            .viewport
            .is_none()
    );
    assert_eq!(ortho_scale(&app, camera), 1.0);
    assert_eq!(ortho_scale(&app, late), 2.0);
    let restored = app
        .world()
        .get::<Camera>(late)
        .unwrap()
        .viewport
        .as_ref()
        .unwrap();
    assert_eq!(restored.physical_size, UVec2::splat(256));
    assert_eq!(restored.depth, 0.1..0.9);
}

#[test]
fn observers_receive_one_relayout_per_new_version_and_select_marked_roots() {
    #[derive(Resource, Default)]
    struct Seen(Vec<(LayoutVersion, Vec2, usize)>);

    let (mut app, window) = viewport_app(default(), UVec2::new(1280, 720));
    app.add_plugins(LayoutReactivePlugin)
        .insert_resource(ViewportSettleOverride(Some(Duration::ZERO)))
        .init_resource::<Seen>()
        .add_observer(
            |event: On<RelayoutTriggered>,
             mut seen: ResMut<Seen>,
             roots: Query<(), With<LayoutReactive>>| {
                seen.0.push((
                    event.version,
                    event.viewport.logical_size,
                    roots.iter().count(),
                ));
            },
        );
    let root = app.world_mut().spawn(LayoutReactive).id();
    app.world_mut().spawn_empty();
    step(&mut app, 0);
    step(&mut app, 0);
    resize(&mut app, window, 1600, 900, false);
    step(&mut app, 0);
    assert!(app.world().get::<LayoutReactive>(root).is_some());
    assert_eq!(
        app.world().resource::<Seen>().0,
        vec![
            (LayoutVersion(1), Vec2::new(1280.0, 720.0), 1),
            (LayoutVersion(2), Vec2::new(1600.0, 900.0), 1),
        ],
    );
}

#[test]
fn scroll_observer_converts_bounds_clamps_and_bubbles_unhandled_axes() {
    let mut app = App::new();
    // PointerTraversal queries Window even when the render target is not a window.
    app.world_mut().register_component::<Window>();
    app.init_resource::<MouseScrollPixelsPerLine>();
    **app.world_mut().resource_mut::<MouseScrollPixelsPerLine>() = 40.0;
    let parent = app
        .world_mut()
        .spawn((
            Node {
                overflow: Overflow::scroll_x(),
                ..default()
            },
            ScrollPosition::default(),
            ComputedNode {
                size: Vec2::splat(200.0),
                content_size: Vec2::splat(400.0),
                inverse_scale_factor: 0.5,
                ..default()
            },
        ))
        .observe(apply_scroll)
        .id();
    let child = app
        .world_mut()
        .spawn((
            Node {
                overflow: Overflow::scroll_y(),
                ..default()
            },
            ScrollPosition::default(),
            ComputedNode {
                size: Vec2::splat(200.0),
                content_size: Vec2::splat(400.0),
                inverse_scale_factor: 0.5,
                ..default()
            },
            ChildOf(parent),
        ))
        .observe(apply_scroll)
        .id();
    let leaf = app.world_mut().spawn(ChildOf(child)).id();
    let mut scroll = |unit, x, y| {
        app.world_mut().trigger(PointerScroll {
            entity: leaf,
            pointer: Pointer::new(
                PointerId::Mouse,
                Location {
                    position: Vec2::ZERO,
                    target: RenderTarget::None { size: UVec2::ONE }
                        .normalize(None)
                        .unwrap(),
                },
            ),
            unit,
            x,
            y,
            hit: HitData::new(Entity::PLACEHOLDER, 0.0, None, None),
            phase: TouchPhase::Moved,
        });
        (
            app.world().get::<ScrollPosition>(parent).unwrap().0,
            app.world().get::<ScrollPosition>(child).unwrap().0,
        )
    };
    assert_eq!(
        scroll(MouseScrollUnit::Line, 0.0, -1.0),
        (Vec2::ZERO, Vec2::new(0.0, 40.0)),
    );
    assert_eq!(
        scroll(MouseScrollUnit::Pixel, -50.0, 0.0),
        (Vec2::new(25.0, 0.0), Vec2::new(0.0, 40.0)),
    );
    assert_eq!(
        scroll(MouseScrollUnit::Pixel, -25.0, -300.0),
        (Vec2::new(25.0, 0.0), Vec2::new(0.0, 100.0)),
    );
    assert_eq!(
        scroll(MouseScrollUnit::Line, 0.0, 3.0),
        (Vec2::new(25.0, 0.0), Vec2::ZERO),
    );
}

#[test]
fn a_toast_and_its_children_despawn_at_the_deadline() {
    let mut app = App::new();
    app.init_resource::<Time>().add_plugins(ToastPlugin);
    let toast = app.world_mut().spawn(ToastTimer::from_secs(0.25)).id();
    let child = app.world_mut().spawn(ChildOf(toast)).id();
    let other = app.world_mut().spawn_empty().id();
    app.world_mut()
        .resource_mut::<Time>()
        .advance_by(Duration::from_millis(249));
    app.update();
    assert!(app.world().get_entity(toast).is_ok());
    assert!(app.world().get_entity(child).is_ok());
    app.world_mut()
        .resource_mut::<Time>()
        .advance_by(Duration::from_millis(1));
    app.update();
    assert!(app.world().get_entity(toast).is_err());
    assert!(app.world().get_entity(child).is_err());
    assert!(app.world().get_entity(other).is_ok());
}

#[test]
fn action_ids_store_borrowed_owned_and_typed_labels_as_components() {
    #[derive(Debug, PartialEq, Eq)]
    enum Action {
        Open,
    }
    impl UiActionLabel for Action {
        fn label(&self) -> &str {
            "open"
        }
    }

    let mut world = World::new();
    let id = world
        .spawn((UiActionId(Cow::Borrowed("save")), UiActionId(Action::Open)))
        .id();
    let owned = world
        .spawn(UiActionId(Cow::<str>::Owned(String::from("exit"))))
        .id();
    assert_eq!(world.get::<UiActionId>(id).unwrap().label(), "save");
    assert_eq!(world.get::<UiActionId<Action>>(id).unwrap().0, Action::Open);
    assert_eq!(world.get::<UiActionId<Action>>(id).unwrap().label(), "open");
    assert_eq!(world.get::<UiActionId>(owned).unwrap().label(), "exit");
}

#[test]
fn window_config_reaches_the_primary_window_and_fullscreen_override_is_force_on() {
    let config = WindowBootstrapConfig {
        title: "Example".into(),
        size: UVec2::new(900, 600),
        min_size: Vec2::new(450.0, 300.0),
        resizable: false,
        position: WindowPosition::At(IVec2::new(10, 20)),
        ..default()
    };
    let mut app = App::new();
    app.add_plugins(build_window_plugin(&config));
    let world = app.world_mut();
    let window = world
        .query_filtered::<&Window, With<PrimaryWindow>>()
        .single(world)
        .unwrap();
    assert_eq!(window.title, "Example");
    assert_eq!(window.physical_size(), UVec2::new(900, 600));
    assert_eq!(window.resize_constraints.min_width, 450.0);
    assert_eq!(window.resize_constraints.min_height, 300.0);
    assert_eq!(window.position, WindowPosition::At(IVec2::new(10, 20)));
    assert!(!window.resizable);
    assert!(matches!(window.mode, WindowMode::Windowed));
    let fullscreen = apply_fullscreen_override(config, true);
    let unchanged = apply_fullscreen_override(fullscreen, false);
    assert!(matches!(
        build_window_descriptor(&unchanged).mode,
        WindowMode::BorderlessFullscreen(MonitorSelection::Primary),
    ));
}

#[test]
fn headless_bootstrap_disables_window_exit_and_uses_requested_tick_rate() {
    use bevy::app::RunMode;

    let mut config = WindowBootstrapConfig {
        headless: true,
        headless_tick_rate_hz: 50.0,
        ..default()
    };
    let plugin = build_window_plugin(&config);
    assert!(plugin.primary_window.is_none());
    assert!(matches!(plugin.exit_condition, ExitCondition::DontExit));
    assert!(matches!(
        build_headless_schedule_runner(&config).run_mode,
        RunMode::Loop { wait: Some(wait) } if wait == Duration::from_millis(20),
    ));
    config.headless_tick_rate_hz = f64::NAN;
    assert!(matches!(
        build_headless_schedule_runner(&config).run_mode,
        RunMode::Loop { wait: Some(wait) } if wait == Duration::from_secs_f64(1.0 / 60.0),
    ));
}

#[cfg(feature = "serde")]
#[test]
fn display_preferences_round_trip_through_serde() {
    let settings = DisplaySettings {
        ui_scale: UiScaleChoice::Custom(1.125),
    };
    let json = serde_json::to_string(&settings).unwrap();
    assert_eq!(
        serde_json::from_str::<DisplaySettings>(&json).unwrap(),
        settings
    );
}

#[cfg(feature = "rkyv")]
#[test]
fn display_preferences_round_trip_through_rkyv() {
    let settings = DisplaySettings {
        ui_scale: UiScaleChoice::Custom(1.125),
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&settings).unwrap();
    assert_eq!(
        rkyv::from_bytes::<DisplaySettings, rkyv::rancor::Error>(&bytes).unwrap(),
        settings,
    );
}
