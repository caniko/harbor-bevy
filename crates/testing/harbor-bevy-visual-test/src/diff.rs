//! Masked grayscale MSSIM comparisons. Source screenshots are never modified.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mask {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Mask {
    pub(crate) fn fits(self, width: u32, height: u32) -> bool {
        self.w != 0
            && self.h != 0
            && self.x.checked_add(self.w).is_some_and(|end| end <= width)
            && self.y.checked_add(self.h).is_some_and(|end| end <= height)
    }
}

/// Paint rectangular exclusions opaque black, clipping to image bounds.
pub fn apply_masks(image: &mut RgbaImage, masks: &[Mask]) {
    let (width, height) = image.dimensions();
    for &Mask { x, y, w, h } in masks {
        for row in y..y.saturating_add(h).min(height) {
            for col in x..x.saturating_add(w).min(width) {
                image.put_pixel(col, row, Rgba([0, 0, 0, 255]));
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum SnapshotOptions {
    Compare { threshold: f64 },
    Update,
}

impl SnapshotOptions {
    pub fn compare(threshold: f64) -> Self {
        Self::Compare { threshold }
    }

    pub fn update() -> Self {
        Self::Update
    }

    pub fn from_feature(threshold: f64) -> Self {
        if cfg!(feature = "update-snapshots") {
            Self::Update
        } else {
            Self::compare(threshold)
        }
    }
}

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("baseline missing at {0}; update the snapshot explicitly")]
    BaselineMissing(PathBuf),
    #[error("size mismatch: baseline {bw}x{bh}, actual {aw}x{ah}")]
    SizeMismatch { bw: u32, bh: u32, aw: u32, ah: u32 },
    #[error("MSSIM {score:.4} below threshold {threshold:.4} (diff: {diff_path})")]
    BelowThreshold {
        score: f64,
        threshold: f64,
        diff_path: PathBuf,
    },
    #[error("snapshot threshold must be between 0 and 1, got {0}")]
    InvalidThreshold(f64),
    #[error(transparent)]
    Image(#[from] image::ImageError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Compare(#[from] image_compare::CompareError),
}

pub fn assert_snapshot(
    baseline: &Path,
    actual: &Path,
    threshold: f64,
) -> Result<(), SnapshotError> {
    compare_snapshot(
        baseline,
        actual,
        &[],
        SnapshotOptions::from_feature(threshold),
    )
    .map(|_| ())
}

/// Returns `None` after a baseline update. A failed comparison writes a
/// PNG or WebP color map beside the actual screenshot.
pub fn compare_snapshot(
    baseline: &Path,
    actual: &Path,
    masks: &[Mask],
    options: SnapshotOptions,
) -> Result<Option<f64>, SnapshotError> {
    let SnapshotOptions::Compare { threshold } = options else {
        save_image_atomic(&image::open(actual)?, baseline)?;
        return Ok(None);
    };
    if !(0.0..=1.0).contains(&threshold) {
        return Err(SnapshotError::InvalidThreshold(threshold));
    }
    let mut expected = match image::open(baseline) {
        Err(image::ImageError::IoError(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Err(SnapshotError::BaselineMissing(baseline.to_owned()));
        }
        result => result?.into_rgba8(),
    };
    let mut captured = image::open(actual)?.into_rgba8();
    let (bw, bh) = expected.dimensions();
    let (aw, ah) = captured.dimensions();
    if (bw, bh) != (aw, ah) {
        return Err(SnapshotError::SizeMismatch { bw, bh, aw, ah });
    }
    apply_masks(&mut expected, masks);
    apply_masks(&mut captured, masks);
    let result = image_compare::gray_similarity_structure(
        &image_compare::Algorithm::MSSIMSimple,
        &DynamicImage::from(expected).into_luma8(),
        &DynamicImage::from(captured).into_luma8(),
    )?;
    if result.score < threshold {
        let diff_path = actual.with_extension(
            if ImageFormat::from_path(actual).ok() == Some(ImageFormat::WebP) {
                "diff.webp"
            } else {
                "diff.png"
            },
        );
        save_image_atomic(&result.image.to_color_map(), &diff_path)?;
        return Err(SnapshotError::BelowThreshold {
            score: result.score,
            threshold,
            diff_path,
        });
    }
    Ok(Some(result.score))
}

pub(crate) fn save_image_atomic(image: &DynamicImage, path: &Path) -> image::ImageResult<()> {
    let parent = crate::parent_dir(path);
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    image.write_to(file.as_file_mut(), ImageFormat::from_path(path)?)?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}
