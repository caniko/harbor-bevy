//! Deterministic geometry, text, and pixel audits over caller-supplied observations.

use image::RgbaImage;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::Mask;

/// Pixel coordinates with a top-left origin.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuditRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl AuditRect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    fn right(self) -> f32 {
        self.x + self.w
    }
    fn bottom(self) -> f32 {
        self.y + self.h
    }

    fn is_valid(self) -> bool {
        [self.x, self.y, self.w, self.h, self.right(), self.bottom()]
            .iter()
            .all(|value| value.is_finite())
            && self.w > 0.0
            && self.h > 0.0
    }

    fn overlaps(self, other: Self) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

/// An observed visible node, not an expected node from the fixture definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuditNode {
    pub label: String,
    pub text: Option<String>,
    pub rect: AuditRect,
}

impl AuditNode {
    pub fn text(label: impl Into<String>, text: impl Into<String>, rect: AuditRect) -> Self {
        Self {
            label: label.into(),
            text: Some(text.into()),
            rect,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuditInput {
    pub width: u32,
    pub height: u32,
    pub nodes: Vec<AuditNode>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NonBlankRegion {
    pub rect: Mask,
    /// Minimum fraction of pixels more than eight luma levels from their region mean.
    pub min_ink_fraction: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum AuditCheck {
    Bounds,
    /// Check all pairs with the given labels. Missing labels fail the check.
    NoOverlap {
        first: String,
        second: String,
    },
    /// Text queries match case-sensitive substrings of a visible node's text.
    RequireText(String),
    ForbidText(String),
    NonBlank(NonBlankRegion),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AuditSpec {
    pub checks: Vec<AuditCheck>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditReport {
    pub checked: usize,
    /// One entry per failed check, in specification order.
    pub failures: Vec<String>,
}

impl AuditReport {
    pub fn assert_pass(&self) -> Result<(), AuditError> {
        if self.failures.is_empty() {
            Ok(())
        } else {
            Err(AuditError::Failed(self.failures.clone()))
        }
    }
}

#[derive(Debug, Error)]
pub enum AuditError {
    #[error("invalid audit input or specification: {0}")]
    Invalid(String),
    #[error("visual audit failed: {}", .0.join("; "))]
    Failed(Vec<String>),
}

impl AuditSpec {
    pub fn needs_nodes(&self) -> bool {
        self.checks
            .iter()
            .any(|check| !matches!(check, AuditCheck::NonBlank(_)))
    }

    pub fn evaluate(
        &self,
        input: &AuditInput,
        image: Option<&RgbaImage>,
    ) -> Result<AuditReport, AuditError> {
        let (width, height) = (input.width, input.height);
        if width == 0 || height == 0 || self.checks.is_empty() {
            return Err(AuditError::Invalid(
                "viewport and checks must be nonempty".into(),
            ));
        }
        if image.is_some_and(|image| image.dimensions() != (width, height)) {
            return Err(AuditError::Invalid(
                "screenshot does not match viewport".into(),
            ));
        }
        if input
            .nodes
            .iter()
            .any(|node| !node.rect.is_valid() || node.label.trim().is_empty())
        {
            return Err(AuditError::Invalid(
                "node has invalid geometry or label".into(),
            ));
        }
        let mut failures = Vec::new();
        let has_text = |needle: &str| {
            input.nodes.iter().any(|node| {
                node.text
                    .as_deref()
                    .is_some_and(|text| text.contains(needle))
            })
        };
        for check in &self.checks {
            let failure = match check {
                AuditCheck::Bounds => {
                    let outside: Vec<_> = input
                        .nodes
                        .iter()
                        .filter(|node| {
                            let rect = node.rect;
                            rect.x < 0.0
                                || rect.y < 0.0
                                || f64::from(rect.right()) > f64::from(width)
                                || f64::from(rect.bottom()) > f64::from(height)
                        })
                        .map(|node| node.label.as_str())
                        .collect();
                    if input.nodes.is_empty() {
                        Some("no nodes to check for viewport bounds".into())
                    } else if outside.is_empty() {
                        None
                    } else {
                        Some(format!("nodes outside viewport: {}", outside.join(", ")))
                    }
                }
                AuditCheck::NoOverlap { first, second } => {
                    if first == second || first.trim().is_empty() || second.trim().is_empty() {
                        return Err(AuditError::Invalid(
                            "overlap check needs two distinct labels".into(),
                        ));
                    }
                    let a: Vec<_> = input
                        .nodes
                        .iter()
                        .filter(|node| node.label == *first)
                        .collect();
                    let b: Vec<_> = input
                        .nodes
                        .iter()
                        .filter(|node| node.label == *second)
                        .collect();
                    if a.is_empty() || b.is_empty() {
                        Some(format!(
                            "missing node for overlap check: {first:?} or {second:?}"
                        ))
                    } else {
                        // ponytail: explicit label-pair checks are quadratic in matching nodes;
                        // use a spatial index if large repeated lists need these audits.
                        a.iter()
                            .any(|a| b.iter().any(|b| a.rect.overlaps(b.rect)))
                            .then(|| format!("{first:?} overlaps {second:?}"))
                    }
                }
                AuditCheck::RequireText(text) | AuditCheck::ForbidText(text) => {
                    if text.trim().is_empty() {
                        return Err(AuditError::Invalid("text query is empty".into()));
                    }
                    let required = matches!(check, AuditCheck::RequireText(_));
                    (has_text(text) != required).then(|| {
                        if required {
                            format!("missing required text {text:?}")
                        } else {
                            format!("found forbidden text {text:?}")
                        }
                    })
                }
                AuditCheck::NonBlank(region) => {
                    if !region.rect.fits(width, height)
                        || !(0.0..=1.0).contains(&region.min_ink_fraction)
                    {
                        return Err(AuditError::Invalid(
                            "invalid non-blank region or ink threshold".into(),
                        ));
                    }
                    let image = image.ok_or_else(|| {
                        AuditError::Invalid("non-blank region requires screenshot pixels".into())
                    })?;
                    let fraction = ink_fraction(image, region.rect);
                    (fraction < region.min_ink_fraction).then(|| {
                        format!(
                            "ink fraction {fraction:.4} below {:.4} in {:?}",
                            region.min_ink_fraction, region.rect
                        )
                    })
                }
            };
            failures.extend(failure);
        }
        Ok(AuditReport {
            checked: self.checks.len(),
            failures,
        })
    }
}

fn ink_fraction(image: &RgbaImage, Mask { x, y, w, h }: Mask) -> f64 {
    let luminance = || {
        (y..y + h).flat_map(|row| {
            (x..x + w).map(move |col| {
                let [r, g, b, a] = image.get_pixel(col, row).0.map(f64::from);
                (0.2126 * r + 0.7152 * g + 0.0722 * b) * a / 255.0
            })
        })
    };
    let area = f64::from(w) * f64::from(h);
    let mean = luminance().sum::<f64>() / area;
    luminance().filter(|luma| (luma - mean).abs() > 8.0).count() as f64 / area
}
