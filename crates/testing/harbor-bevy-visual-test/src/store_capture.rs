//! Capture caller-selected fixtures and derive explicitly specified store crops.

use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};

use image::{ImageFormat, RgbaImage, imageops};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    Mask, VisualSuite,
    capture::{CaptureConfig, CaptureError, FrameSettleHook, capture_with_hook, headless_app},
    diff::save_image_atomic,
};

pub const CAPTURE_LOG_NAME: &str = "capture-log.toml";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreCropSpec {
    /// Single PNG or WebP filename in the output directory.
    pub output: String,
    pub region: Mask,
    /// Optional resize, `[width, height]` in pixels.
    pub output_size: Option<[u32; 2]>,
}

impl StoreCropSpec {
    pub fn apply(&self, image: &RgbaImage) -> Result<RgbaImage, StoreCaptureError> {
        if !self.region.fits(image.width(), image.height())
            || self.output_size.is_some_and(|[w, h]| w == 0 || h == 0)
        {
            return Err(StoreCaptureError::Spec(format!(
                "invalid region or output size for {:?}",
                self.output
            )));
        }
        let Mask { x, y, w, h } = self.region;
        let result = imageops::crop_imm(image, x, y, w, h).to_image();
        Ok(match self.output_size {
            Some([width, height]) if (width, height) != (w, h) => {
                imageops::resize(&result, width, height, imageops::FilterType::Lanczos3)
            }
            _ => result,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreCaptureSpec {
    pub fixture: String,
    pub viewport: String,
    pub output: String,
    #[serde(default)]
    pub crops: Vec<StoreCropSpec>,
}

#[derive(Clone, Debug)]
pub struct StoreCaptureOptions {
    pub output_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreArtifact {
    pub fixture: String,
    pub viewport: String,
    /// Relative filename of the source frame, absent for the original capture.
    pub source: Option<String>,
    pub output: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreCaptureLog {
    pub artifacts: Vec<StoreArtifact>,
}

impl StoreCaptureLog {
    pub fn read(output_dir: &Path) -> Result<Self, StoreCaptureError> {
        Ok(toml::from_str(&fs::read_to_string(
            output_dir.join(CAPTURE_LOG_NAME),
        )?)?)
    }

    pub fn write(&self, output_dir: &Path) -> Result<(), StoreCaptureError> {
        use std::io::Write;

        fs::create_dir_all(output_dir)?;
        let mut file = tempfile::NamedTempFile::new_in(output_dir)?;
        file.write_all(toml::to_string_pretty(self)?.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(output_dir.join(CAPTURE_LOG_NAME))
            .map_err(|error| error.error)?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum StoreCaptureError {
    #[error("invalid store capture: {0}")]
    Spec(String),
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Image(#[from] image::ImageError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    TomlDecode(#[from] toml::de::Error),
    #[error(transparent)]
    TomlEncode(#[from] toml::ser::Error),
}

/// Store captures are unmasked. Only filenames and requested pixel regions
/// determine outputs; no store-specific art or layout is synthesized.
pub fn run_store_capture(
    suite: &impl VisualSuite,
    specs: &[StoreCaptureSpec],
    options: &StoreCaptureOptions,
    settle: Option<&FrameSettleHook<'_>>,
) -> Result<StoreCaptureLog, StoreCaptureError> {
    let fixtures = suite.fixtures();
    let mut paths = BTreeSet::new();
    let mut jobs = Vec::new();
    if specs.is_empty() {
        return Err(StoreCaptureError::Spec(
            "no store captures requested".into(),
        ));
    }
    for spec in specs {
        let mut matching = fixtures
            .iter()
            .filter(|fixture| fixture.name == spec.fixture);
        let fixture = matching
            .next()
            .filter(|_| matching.next().is_none())
            .ok_or_else(|| {
                StoreCaptureError::Spec(format!("unknown or ambiguous fixture {:?}", spec.fixture))
            })?;
        fixture.validate().map_err(StoreCaptureError::Spec)?;
        let viewport = fixture
            .viewports
            .iter()
            .find(|viewport| viewport.label == spec.viewport)
            .ok_or_else(|| {
                StoreCaptureError::Spec(format!(
                    "unknown viewport {:?} in {:?}",
                    spec.viewport, spec.fixture
                ))
            })?;
        for output in
            std::iter::once(&spec.output).chain(spec.crops.iter().map(|crop| &crop.output))
        {
            if !crate::safe_id(output)
                || !paths.insert(output.to_ascii_lowercase())
                || !matches!(
                    ImageFormat::from_path(output),
                    Ok(ImageFormat::Png | ImageFormat::WebP)
                )
            {
                return Err(StoreCaptureError::Spec(format!(
                    "invalid or duplicate output {output:?}"
                )));
            }
        }
        for crop in &spec.crops {
            if !crop.region.fits(viewport.width, viewport.height)
                || crop.output_size.is_some_and(|[w, h]| w == 0 || h == 0)
            {
                return Err(StoreCaptureError::Spec(format!(
                    "invalid crop {:?}",
                    crop.output
                )));
            }
        }
        jobs.push((spec, fixture, viewport));
    }

    fs::create_dir_all(&options.output_dir)?;
    // Stage all frames and crops first, so a render/encode failure leaves old outputs alone.
    let stage = tempfile::tempdir_in(&options.output_dir)?;
    let mut log = StoreCaptureLog::default();
    for (spec, fixture, viewport) in jobs {
        let mut app = headless_app();
        suite.build(fixture, &mut app);
        let capture_path = stage.path().join(&spec.output);
        let config = CaptureConfig::for_fixture(fixture, viewport, &capture_path);
        capture_with_hook(&mut app, &config, settle)?;
        let frame = image::open(&capture_path)?.into_rgba8();
        let artifact = |output, source, (width, height)| StoreArtifact {
            fixture: fixture.name.clone(),
            viewport: viewport.label.clone(),
            source,
            output,
            width,
            height,
        };
        log.artifacts
            .push(artifact(spec.output.clone(), None, frame.dimensions()));
        for crop in &spec.crops {
            let image = crop.apply(&frame)?;
            let size = image.dimensions();
            save_image_atomic(&image.into(), &stage.path().join(&crop.output))?;
            log.artifacts.push(artifact(
                crop.output.clone(),
                Some(spec.output.clone()),
                size,
            ));
        }
    }
    log.artifacts.sort_by(|a, b| a.output.cmp(&b.output));
    for artifact in &log.artifacts {
        fs::rename(
            stage.path().join(&artifact.output),
            options.output_dir.join(&artifact.output),
        )?;
    }
    log.write(&options.output_dir)?;
    Ok(log)
}
