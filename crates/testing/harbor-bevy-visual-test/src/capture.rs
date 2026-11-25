//! Offscreen capture using Bevy's image render target and screenshot readback.
//!
//! See Bevy's [headless renderer example](https://docs.rs/crate/bevy/0.20.1/source/examples/app/headless_renderer.rs)
//! for the windowless render-target setup.

use std::{
    fs, io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use bevy::{
    app::{PluginsState, PostStartup},
    camera::{CameraPlugin, CameraUpdateSystems, RenderTarget},
    input_focus::{InputDispatchPlugin, InputFocusPlugin},
    prelude::*,
    render::{
        RenderPlugin,
        render_resource::{TextureFormat, TextureUsages},
        renderer::RenderDevice,
        view::screenshot::{Screenshot, ScreenshotCaptured, save_to_disk},
    },
    time::TimeUpdateStrategy,
    ui::IsDefaultUiCamera,
    window::{ExitCondition, WindowPlugin},
};
use image::ImageFormat;
use thiserror::Error;

use crate::{
    FixtureSpec, Resolution,
    gpu_pin::{self, GpuPinError},
};

#[derive(Clone, Debug)]
pub struct CaptureConfig {
    pub width: u32,
    pub height: u32,
    pub warmup_frames: u32,
    pub flush_frames: u32,
    pub flush_timeout: Duration,
    pub out_path: PathBuf,
    /// Simulated time in units of one 60 Hz frame per update; zero freezes time.
    pub time_speed: f32,
}

impl CaptureConfig {
    pub fn new(out_path: impl Into<PathBuf>) -> Self {
        Self {
            width: 1280,
            height: 720,
            warmup_frames: 30,
            flush_frames: 4,
            flush_timeout: Duration::from_secs(5),
            out_path: out_path.into(),
            time_speed: 0.0,
        }
    }

    pub fn for_fixture(
        fixture: &FixtureSpec,
        viewport: &Resolution,
        path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            width: viewport.width,
            height: viewport.height,
            warmup_frames: fixture.warmup_frames,
            flush_frames: fixture.flush_frames,
            time_speed: fixture.time_speed,
            ..Self::new(path)
        }
    }
}

/// Available before the settle hook and during Startup, for callers with
/// camera-specific setup. Cameras are otherwise retargeted automatically.
#[derive(Resource, Clone, Debug)]
pub struct CaptureTarget(pub Handle<Image>);

/// Called after the target and simulation clock are installed, before warmup.
pub type FrameSettleHook<'a> = dyn Fn(&mut App, &CaptureTarget) + 'a;

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("invalid capture configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("Bevy plugins did not become ready")]
    PluginsNotReady,
    #[error("offscreen capture requires a Bevy renderer and image assets")]
    MissingRenderer,
    #[error("no active camera targeting the capture image after warmup")]
    MissingCamera,
    #[error("screenshot did not arrive within the flush timeout: {0}")]
    Timeout(PathBuf),
    #[error("screenshot data at {0} has unexpected dimensions")]
    WrongDimensions(PathBuf),
    #[error(transparent)]
    Gpu(#[from] GpuPinError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Image(#[from] image::ImageError),
}

/// Install stock 2D/UI plugins explicitly so a consumer's Bevy features
/// cannot implicitly add a window backend or pipelined rendering.
pub fn headless_app() -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        bevy::transform::TransformPlugin,
        bevy::diagnostic::DiagnosticsPlugin,
        bevy::input::InputPlugin,
        InputFocusPlugin,
        InputDispatchPlugin,
        WindowPlugin {
            primary_window: None,
            exit_condition: ExitCondition::DontExit,
            ..default()
        },
        bevy::a11y::AccessibilityPlugin,
        AssetPlugin::default(),
        RenderPlugin::default(),
        ImagePlugin::default(),
        bevy::mesh::MeshPlugin,
        CameraPlugin,
    ));
    app.add_plugins((
        bevy::core_pipeline::CorePipelinePlugin,
        bevy::sprite::SpritePlugin,
        bevy::sprite_render::SpriteRenderPlugin,
        bevy::text::TextPlugin,
        bevy::ui::UiPlugin,
        bevy::ui_render::UiRenderPlugin,
        bevy::state::app::StatesPlugin,
        bevy::ui_widgets::UiWidgetsPlugins,
        bevy::picking::DefaultPickingPlugins,
    ));
    app
}

/// Finish and clean plugins idempotently before manually driving `App::update`.
pub fn ensure_app_ready(app: &mut App) -> Result<(), CaptureError> {
    let start = Instant::now();
    while app.plugins_state() == PluginsState::Adding {
        if start.elapsed() >= Duration::from_secs(10) {
            return Err(CaptureError::PluginsNotReady);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if app.plugins_state() == PluginsState::Ready {
        app.finish();
    }
    if app.plugins_state() == PluginsState::Finished {
        app.cleanup();
    }
    if !app.world().contains_resource::<Assets<Image>>()
        || !app.world().contains_resource::<RenderDevice>()
    {
        return Err(CaptureError::MissingRenderer);
    }
    gpu_pin::validate_app(app)?;
    Ok(())
}

#[derive(Resource)]
struct RetargetingInstalled;

#[derive(Component)]
struct CaptureDefaultUiCamera;

fn bind_cameras(
    target: Res<CaptureTarget>,
    mut commands: Commands,
    mut cameras: Query<(
        Entity,
        &Camera,
        &mut RenderTarget,
        Has<IsDefaultUiCamera>,
        Has<CaptureDefaultUiCamera>,
    )>,
) {
    let mut explicit = false;
    let mut previous = None;
    let mut fallback = None;
    for (entity, camera, mut render_target, is_default, auto_selected) in &mut cameras {
        if !matches!(&*render_target, RenderTarget::Image(image) if image.handle == target.0) {
            *render_target = RenderTarget::Image(target.0.clone().into());
        }
        if auto_selected {
            previous = Some(entity);
        } else {
            explicit |= is_default;
        }
        if camera.is_active {
            fallback = fallback.max(Some((camera.order, entity)));
        }
    }
    // Bevy's unmarked UI fallback only considers cameras targeting the primary window.
    let selected = if explicit {
        None
    } else {
        fallback.map(|(_, entity)| entity)
    };
    if previous != selected {
        if let Some(entity) = previous {
            commands
                .entity(entity)
                .remove::<(IsDefaultUiCamera, CaptureDefaultUiCamera)>();
        }
        if let Some(entity) = selected {
            commands
                .entity(entity)
                .insert((IsDefaultUiCamera, CaptureDefaultUiCamera));
        }
    }
}

#[cfg(test)]
#[test]
fn offscreen_default_ui_camera_preserves_a_late_explicit_choice() {
    let mut app = App::new();
    app.insert_resource(CaptureTarget(Handle::default()))
        .add_systems(Update, bind_cameras);
    let first = app.world_mut().spawn(Camera::default()).id();
    app.update();
    assert!(app.world().get::<IsDefaultUiCamera>(first).is_some());
    assert!(matches!(
        app.world().get::<RenderTarget>(first),
        Some(RenderTarget::Image(_))
    ));
    let explicit = app
        .world_mut()
        .spawn((Camera::default(), IsDefaultUiCamera))
        .id();
    app.update();
    assert!(app.world().get::<IsDefaultUiCamera>(explicit).is_some());
    assert!(app.world().get::<IsDefaultUiCamera>(first).is_none());
}

pub fn capture_after_frames(app: &mut App, config: &CaptureConfig) -> Result<(), CaptureError> {
    capture_with_hook(app, config, None)
}

/// Capture only after warmup and publish atomically after successful readback.
/// Callers providing their own app must disable pipelined rendering.
pub fn capture_with_hook(
    app: &mut App,
    config: &CaptureConfig,
    hook: Option<&FrameSettleHook<'_>>,
) -> Result<(), CaptureError> {
    if config.width == 0 || config.height == 0 {
        return Err(CaptureError::InvalidConfig("dimensions must be nonzero"));
    }
    if config.flush_timeout.is_zero() {
        return Err(CaptureError::InvalidConfig("flush timeout must be nonzero"));
    }
    let step = Duration::try_from_secs_f32(config.time_speed / 60.0)
        .map_err(|_| CaptureError::InvalidConfig("time speed must be finite and nonnegative"))?;
    let suffix = match ImageFormat::from_path(&config.out_path)? {
        ImageFormat::Png => ".png",
        ImageFormat::WebP => ".webp",
        _ => {
            return Err(CaptureError::InvalidConfig(
                "capture output must be PNG or WebP",
            ));
        }
    };
    ensure_app_ready(app)?;
    let max_size = app
        .world()
        .resource::<RenderDevice>()
        .limits()
        .max_texture_dimension_2d;
    if config.width > max_size || config.height > max_size {
        return Err(CaptureError::InvalidConfig(
            "dimensions exceed the GPU texture limit",
        ));
    }
    let mut image = Image::new_target_texture(
        config.width,
        config.height,
        TextureFormat::Rgba8UnormSrgb,
        None,
    );
    image.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let handle = app.world_mut().resource_mut::<Assets<Image>>().add(image);
    let target = CaptureTarget(handle.clone());
    app.insert_resource(target.clone())
        .insert_resource(TimeUpdateStrategy::ManualDuration(step));
    if !app.world().contains_resource::<RetargetingInstalled>() {
        app.insert_resource(RetargetingInstalled)
            .add_systems(PostStartup, bind_cameras.before(CameraUpdateSystems))
            .add_systems(PostUpdate, bind_cameras.before(CameraUpdateSystems));
    }
    if let Some(hook) = hook {
        hook(app, &target);
    }
    for _ in 0..config.warmup_frames.max(1) {
        app.update();
    }
    let world = app.world_mut();
    if !world
        .query::<(&Camera, &RenderTarget)>()
        .iter(world)
        .any(|(camera, target)| {
            camera.is_active
                && matches!(target, RenderTarget::Image(image) if image.handle == handle)
        })
    {
        return Err(CaptureError::MissingCamera);
    }

    let parent = crate::parent_dir(&config.out_path);
    fs::create_dir_all(parent)?;
    let temp = tempfile::Builder::new()
        .suffix(suffix)
        .tempfile_in(parent)?;
    let mut save = save_to_disk(temp.path().to_owned());
    let saved = Arc::new(AtomicBool::new(false));
    let done = saved.clone();
    let request = world
        .spawn(Screenshot::image(handle))
        .observe(move |event: On<ScreenshotCaptured>| {
            save(event);
            done.store(true, Ordering::Release);
        })
        .id();

    let start = Instant::now();
    for frame in 0.. {
        if frame >= config.flush_frames && saved.load(Ordering::Acquire) {
            break;
        }
        if start.elapsed() >= config.flush_timeout {
            app.world_mut().despawn(request);
            return Err(CaptureError::Timeout(config.out_path.clone()));
        }
        app.update();
        if !saved.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let captured = image::open(temp.path())?;
    if (captured.width(), captured.height()) != (config.width, config.height) {
        return Err(CaptureError::WrongDimensions(config.out_path.clone()));
    }
    temp.persist(&config.out_path)
        .map_err(|error| error.error)?;
    Ok(())
}
