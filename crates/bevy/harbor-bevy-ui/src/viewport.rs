//! Settled UI metrics and optional camera letterboxing for the primary window.

use std::{collections::HashMap, time::Duration};

use bevy::{
    camera::{RenderTarget, Viewport},
    prelude::*,
    text::{DEFAULT_REM_SIZE_PX, FontSize},
    time::Real,
    window::{PrimaryWindow, WindowResized},
};

use crate::pointer_router::LogicalRect;

const REFERENCE_SIZE: Vec2 = Vec2::new(1280.0, 720.0);
const DEFAULT_SETTLE: Duration = Duration::from_millis(120);

/// Insets from the drawable area's edges, in logical pixels.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct SafeAreaInsets {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl SafeAreaInsets {
    /// Usable bounds relative to the drawable area's origin. Negative and NaN
    /// insets count as zero; oversized insets collapse the axis without inversion.
    pub fn usable_rect(self, logical_size: Vec2) -> LogicalRect {
        fn axis(size: f32, leading: f32, trailing: f32) -> (f32, f32) {
            let size = if size.is_finite() { size.max(0.0) } else { 0.0 };
            let start = leading.max(0.0).min(size);
            (start, (size - trailing.max(0.0)).max(start))
        }

        let (left, right) = axis(logical_size.x, self.left, self.right);
        let (top, bottom) = axis(logical_size.y, self.top, self.bottom);
        LogicalRect {
            min: [left, top],
            max: [right, bottom],
        }
    }
}

/// Sizing policy for the primary viewport. Initialize before adding
/// [`ViewportPlugin`], or modify it while the app is running.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct ViewportConfig {
    pub target_aspect: f32,
    pub settle_delay_secs: f64,
    /// Logical-pixel tolerance, applied independently to each axis.
    pub resize_tolerance_px: f32,
    /// Design dimensions in logical pixels, used for responsive scaling.
    pub reference_size: Vec2,
    /// Either axis at or below this threshold is compact.
    pub compact_max_size: Vec2,
    /// Both axes at or above these thresholds are spacious, unless compact.
    pub spacious_min_size: Vec2,
    /// Fit the primary window to `target_aspect` whether windowed or fullscreen.
    pub enable_letterboxing: bool,
}

impl Default for ViewportConfig {
    fn default() -> Self {
        Self {
            target_aspect: 16.0 / 9.0,
            settle_delay_secs: 0.12,
            resize_tolerance_px: 1.0,
            reference_size: REFERENCE_SIZE,
            compact_max_size: Vec2::new(1280.0, 600.0),
            spacious_min_size: Vec2::new(2400.0, 1200.0),
            enable_letterboxing: false,
        }
    }
}

impl ViewportConfig {
    fn reference_size(&self) -> Vec2 {
        if self.reference_size.is_finite() && self.reference_size.min_element() > 0.0 {
            self.reference_size
        } else {
            REFERENCE_SIZE
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayoutDensity {
    Compact,
    #[default]
    Standard,
    Spacious,
}

/// User preference, independent of the window's DPI scale.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub enum UiScaleChoice {
    Scale75,
    #[default]
    Scale100,
    Scale125,
    Scale150,
    Custom(f32),
}

impl UiScaleChoice {
    pub fn multiplier(self) -> f32 {
        match self {
            Self::Scale75 => 0.75,
            Self::Scale125 => 1.25,
            Self::Scale150 => 1.5,
            Self::Custom(scale) if scale.is_finite() && scale > 0.0 => scale,
            Self::Scale100 | Self::Custom(_) => 1.0,
        }
    }
}

/// Serializable UI preferences, not transient window measurements.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct DisplaySettings {
    pub ui_scale: UiScaleChoice,
}

/// A snapshot of the drawable area, excluding any letterbox bars.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportMetrics {
    pub logical_size: Vec2,
    pub physical_size: UVec2,
    /// Physical pixels per logical pixel as reported by Bevy.
    pub scale_factor: f32,
    pub density: LayoutDensity,
    pub user_scale: f32,
    /// Responsive design scale times the user scale. Does not include DPI:
    /// Bevy applies DPI when mapping logical UI coordinates to the window.
    pub effective_scale: f32,
}

impl ViewportMetrics {
    /// Usable logical bounds relative to this viewport's drawable origin.
    pub fn usable_rect(&self, insets: SafeAreaInsets) -> LogicalRect {
        insets.usable_rect(self.logical_size)
    }

    /// Build a snapshot from the drawable physical size and its DPI scale.
    /// Returns `None` for invalid scale or a minimized viewport.
    pub fn from_physical_size(
        physical_size: UVec2,
        scale_factor: f32,
        config: &ViewportConfig,
        settings: &DisplaySettings,
    ) -> Option<Self> {
        if physical_size.min_element() == 0 || !scale_factor.is_finite() || scale_factor <= 0.0 {
            return None;
        }
        let logical_size = physical_size.as_vec2() / scale_factor;
        if !logical_size.is_finite() || logical_size.min_element() <= 0.0 {
            return None;
        }
        let density = if logical_size.cmple(config.compact_max_size).any() {
            LayoutDensity::Compact
        } else if logical_size.cmpge(config.spacious_min_size).all() {
            LayoutDensity::Spacious
        } else {
            LayoutDensity::Standard
        };
        let user_scale = settings.ui_scale.multiplier();
        let effective_scale = (logical_size / config.reference_size()).min_element() * user_scale;
        Some(Self {
            logical_size,
            physical_size,
            scale_factor,
            density,
            user_scale,
            effective_scale,
        })
    }

    /// Convert design pixels to a responsive font size for Bevy's default
    /// `RemSize`. Do not also apply `effective_scale` to the root rem size.
    pub fn font_size_rem(&self, design_px: f32) -> FontSize {
        FontSize::Rem((design_px / DEFAULT_REM_SIZE_PX) * self.effective_scale)
    }
}

/// The last settled viewport; retains its metrics while minimized or headless.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct CurrentViewport(pub ViewportMetrics);

impl Default for CurrentViewport {
    fn default() -> Self {
        Self(ViewportMetrics {
            logical_size: REFERENCE_SIZE,
            physical_size: UVec2::new(1280, 720),
            scale_factor: 1.0,
            density: LayoutDensity::Compact,
            user_scale: 1.0,
            effective_scale: 1.0,
        })
    }
}

/// Debounce state for an unsettled primary-window resize.
#[derive(Resource, Debug, Default)]
pub struct PendingViewportResize {
    sample: Option<(ViewportMetrics, Duration)>,
    adopted: bool,
}

impl PendingViewportResize {
    pub fn has_pending(&self) -> bool {
        self.sample.is_some()
    }

    pub fn pending_size(&self) -> Option<Vec2> {
        self.sample.map(|(metrics, _)| metrics.logical_size)
    }
}

/// `None` uses the configured delay; `Some(Duration::ZERO)` disables debounce.
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct ViewportSettleOverride(pub Option<Duration>);

/// Increments when a viewport snapshot is published.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayoutVersion(pub u64);

/// The live letterbox rectangle; unlike [`CurrentViewport`], it is not debounced.
#[derive(Resource, Debug, Default)]
pub struct LetterboxState {
    pub viewport: Option<Viewport>,
}

fn near(a: Vec2, b: Vec2, tolerance: f32) -> bool {
    a == b || (a - b).abs().max_element() < tolerance
}

fn settle_viewport(
    config: Res<ViewportConfig>,
    settings: Res<DisplaySettings>,
    letterbox: Res<LetterboxState>,
    time: Res<Time<Real>>,
    settle_override: Res<ViewportSettleOverride>,
    windows: Query<(Entity, &Window), With<PrimaryWindow>>,
    mut resized: MessageReader<WindowResized>,
    mut pending: ResMut<PendingViewportResize>,
    mut current: ResMut<CurrentViewport>,
    mut version: ResMut<LayoutVersion>,
) {
    let Ok((primary, window)) = windows.single() else {
        resized.clear();
        pending.sample = None;
        return;
    };
    let physical_size = letterbox
        .viewport
        .as_ref()
        .map_or_else(|| window.physical_size(), |viewport| viewport.physical_size);
    let Some(next) = ViewportMetrics::from_physical_size(
        physical_size,
        window.scale_factor(),
        &config,
        &settings,
    ) else {
        resized.clear();
        pending.sample = None;
        return;
    };
    let tolerance = config.resize_tolerance_px.max(0.0);
    let window_size = window.size();
    // A coalesced burst may go through different sizes and return to the same
    // live sample. Honor only bursts confirmed by their final primary notice.
    let mut intermediate_resize = false;
    let mut last_matches_live = false;
    for notice in resized.read().filter(|notice| notice.window == primary) {
        let size = Vec2::new(notice.width, notice.height);
        if size.is_finite() && size.min_element() > 0.0 {
            last_matches_live = near(size, window_size, tolerance);
            intermediate_resize |= !last_matches_live;
        }
    }
    let burst = intermediate_resize && last_matches_live;
    let settle = settle_override.0.unwrap_or_else(|| {
        Duration::try_from_secs_f64(config.settle_delay_secs).unwrap_or(DEFAULT_SETTLE)
    });
    let previous = current.0;
    let immediate = !pending.adopted
        || next.user_scale != previous.user_scale
        || next.scale_factor != previous.scale_factor
        || next.physical_size == previous.physical_size;
    if !immediate {
        if near(next.logical_size, previous.logical_size, tolerance) {
            pending.sample = None;
            return;
        }
        let now = time.elapsed();
        let since = match pending.sample {
            Some((sample, since))
                if !burst && near(sample.logical_size, next.logical_size, tolerance) =>
            {
                since
            }
            _ => {
                pending.sample = Some((next, now));
                now
            }
        };
        if now.saturating_sub(since) < settle {
            return;
        }
    }
    let first = !pending.adopted;
    pending.sample = None;
    pending.adopted = true;
    if first || previous != next {
        current.0 = next;
        version.0 = version.0.wrapping_add(1);
    }
}

fn fit_viewport(size: UVec2, config: &ViewportConfig) -> Option<Viewport> {
    let aspect = config.target_aspect;
    if !config.enable_letterboxing
        || size.min_element() == 0
        || !aspect.is_finite()
        || aspect <= 0.0
    {
        return None;
    }
    let window = size.as_vec2();
    let fitted = if window.x / window.y > aspect {
        UVec2::new((window.y * aspect).round() as u32, size.y)
    } else {
        UVec2::new(size.x, (window.x / aspect).round() as u32)
    }
    .clamp(UVec2::ONE, size);
    (fitted != size).then(|| Viewport {
        physical_position: (size - fitted) / 2,
        physical_size: fitted,
        ..Viewport::default()
    })
}

fn same_viewport(a: Option<&Viewport>, b: Option<&Viewport>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            a.physical_position == b.physical_position
                && a.physical_size == b.physical_size
                && a.depth == b.depth
        }
        (None, None) => true,
        _ => false,
    }
}

fn targets_primary(target: &RenderTarget, primary: Option<Entity>) -> bool {
    primary.is_some()
        && matches!(target, RenderTarget::Window(window) if window.normalize(primary).map(|window| window.entity()) == primary)
}

/// Fit primary-window cameras to the live aspect ratio. Restore their previous
/// viewports when letterboxing ends or when they stop targeting that window.
pub fn update_letterboxing(
    config: Res<ViewportConfig>,
    windows: Query<(Entity, &Window), With<PrimaryWindow>>,
    mut cameras: Query<(Entity, &RenderTarget, &mut Camera)>,
    mut state: ResMut<LetterboxState>,
    mut original: Local<HashMap<Entity, Option<Viewport>>>,
) {
    let primary = windows.single().ok();
    let viewport = primary.and_then(|(_, window)| fit_viewport(window.physical_size(), &config));
    if !same_viewport(state.viewport.as_ref(), viewport.as_ref()) {
        state.viewport = viewport;
    }
    if state.viewport.is_none() && original.is_empty() {
        return;
    }
    for (entity, target, mut camera) in &mut cameras {
        let fit = state
            .viewport
            .as_ref()
            .filter(|_| targets_primary(target, primary.map(|(entity, _)| entity)));
        if let Some(fit) = fit {
            let previous = original
                .entry(entity)
                .or_insert_with(|| camera.viewport.clone());
            let mut fit = fit.clone();
            if let Some(previous) = previous {
                fit.depth = previous.depth.clone();
            }
            if !same_viewport(camera.viewport.as_ref(), Some(&fit)) {
                camera.viewport = Some(fit);
            }
        } else if let Some(previous) = original.remove(&entity) {
            camera.viewport = previous;
        }
    }
    original.retain(|entity, _| cameras.contains(*entity));
}

/// Fit the reference canvas in the letterboxed `Camera2d` viewport. Restores
/// the previous orthographic scale outside letterboxing. Run after
/// [`update_letterboxing`].
pub fn update_camera_scaling(
    config: Res<ViewportConfig>,
    state: Res<LetterboxState>,
    windows: Query<(Entity, &Window), With<PrimaryWindow>>,
    mut cameras: Query<(Entity, &RenderTarget, &mut Projection), With<Camera2d>>,
    mut original: Local<HashMap<Entity, f32>>,
) {
    let primary = windows.single().ok();
    let fit = state.viewport.as_ref().and_then(|viewport| {
        let (_, window) = primary?;
        let logical_size = viewport.physical_size.as_vec2() / window.scale_factor();
        let scale = (config.reference_size() / logical_size).max_element();
        (scale.is_finite() && scale > 0.0).then_some(scale)
    });
    if fit.is_none() && original.is_empty() {
        return;
    }
    for (entity, target, mut projection) in &mut cameras {
        let Projection::Orthographic(ortho) = &*projection else {
            original.remove(&entity);
            continue;
        };
        let scale = if let Some(scale) =
            fit.filter(|_| targets_primary(target, primary.map(|(entity, _)| entity)))
        {
            original.entry(entity).or_insert(ortho.scale);
            Some(scale)
        } else {
            original.remove(&entity)
        };
        if let Some(scale) = scale
            && ortho.scale != scale
            && let Projection::Orthographic(ortho) = &mut *projection
        {
            ortho.scale = scale;
        }
    }
    original.retain(|entity, _| cameras.contains(*entity));
}

/// `Update` set that updates cameras and publishes settled viewport metrics.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ViewportSystems;

/// Tracks the primary window and its cameras. Use with Bevy's `TimePlugin`.
pub struct ViewportPlugin;

impl Plugin for ViewportPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ViewportConfig>()
            .init_resource::<DisplaySettings>()
            .init_resource::<CurrentViewport>()
            .init_resource::<SafeAreaInsets>()
            .init_resource::<PendingViewportResize>()
            .init_resource::<ViewportSettleOverride>()
            .init_resource::<LayoutVersion>()
            .init_resource::<LetterboxState>()
            .add_message::<WindowResized>()
            .add_systems(
                Update,
                (update_letterboxing, update_camera_scaling, settle_viewport)
                    .chain()
                    .in_set(ViewportSystems),
            );
    }
}
