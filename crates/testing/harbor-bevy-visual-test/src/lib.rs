//! Stock Bevy visual testing, with no window server or semantic backend required by default.
//!
//! Implement [`VisualSuite`] to declare fixtures and build each scene into the
//! provided offscreen app. Use [`pipeline::run_pipeline`] in the parent and
//! [`pipeline::run_worker_fixture`] in your executable's worker entry point.
//! That keeps GPU initialization and rendering failures outside the test process.
//! The parent checks audits before comparing baselines and calling any explicitly
//! supplied [`rubric::RubricBackend`]. For single images, use [`capture`] directly.

use std::{collections::BTreeMap, path::Path, time::Duration};

use bevy::prelude::App;
use serde::{Deserialize, Serialize};

pub mod audit;
pub mod capture;
pub mod coverage_store;
pub mod diff;
pub mod gpu_pin;
pub mod pipeline;
pub mod provenance;
pub mod report;
pub mod rubric;
pub mod store_capture;
pub mod worker_sidecar;

pub use audit::AuditSpec;
pub use capture::{CaptureConfig, CaptureTarget};
pub use diff::Mask;
pub use provenance::{
    CaptureManifest, ImageProvenanceReceipt, PreparedImageReceipt, ProvenanceError,
    VisualImageProvenance, verify_prepared_gpu_images,
};
pub use rubric::Rubric;

#[cfg(test)]
mod tests;

pub trait VisualSuite {
    fn fixtures(&self) -> Vec<FixtureSpec>;
    fn build(&self, fixture: &FixtureSpec, app: &mut App);
    fn masks(&self, _fixture: &FixtureSpec) -> Vec<Mask> {
        vec![]
    }
}

/// A caller-defined grouping such as `landscape` or `handheld`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AspectFamily(pub String);

impl From<&str> for AspectFamily {
    fn from(value: &str) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub label: String,
    pub width: u32,
    pub height: u32,
    pub aspect_family: Option<AspectFamily>,
}

impl Resolution {
    pub fn new(label: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            label: label.into(),
            width,
            height,
            aspect_family: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FixtureSpec {
    pub name: String,
    pub viewports: Vec<Resolution>,
    pub warmup_frames: u32,
    pub flush_frames: u32,
    pub ssim_threshold: f64,
    pub time_speed: f32,
    pub surface_ids: Vec<String>,
    pub anchors: Vec<String>,
    pub audit: Option<AuditSpec>,
    pub rubric: Option<Rubric>,
    pub metadata: BTreeMap<String, String>,
}

impl FixtureSpec {
    pub fn new(name: impl Into<String>, viewports: Vec<Resolution>) -> Self {
        Self {
            name: name.into(),
            viewports,
            warmup_frames: 30,
            flush_frames: 4,
            ssim_threshold: 0.98,
            time_speed: 0.0,
            surface_ids: Vec::new(),
            anchors: Vec::new(),
            audit: None,
            rubric: None,
            metadata: BTreeMap::new(),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        use std::collections::BTreeSet;

        if !safe_id(&self.name) || self.viewports.is_empty() {
            return Err(format!(
                "invalid fixture name or empty viewport list: {:?}",
                self.name
            ));
        }
        let mut seen = BTreeSet::new();
        for viewport in &self.viewports {
            if !safe_id(&viewport.label)
                || !seen.insert(viewport.label.to_ascii_lowercase())
                || viewport.width == 0
                || viewport.height == 0
            {
                return Err(format!(
                    "invalid or repeated viewport {:?} in {:?}",
                    viewport.label, self.name
                ));
            }
        }
        if !(0.0..=1.0).contains(&self.ssim_threshold)
            || Duration::try_from_secs_f32(self.time_speed / 60.0).is_err()
            || self
                .surface_ids
                .iter()
                .chain(&self.anchors)
                .any(|id| id.trim().is_empty())
        {
            return Err(format!(
                "invalid threshold, time speed, surface, or anchor in {:?}",
                self.name
            ));
        }
        Ok(())
    }
}

// Fixture and artifact IDs become filenames. These are portable, single path components.
fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('.')
        && !id.ends_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}
