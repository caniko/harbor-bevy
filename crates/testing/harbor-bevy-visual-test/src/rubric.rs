//! Backend-independent semantic scoring. No evaluation happens without a supplied backend.

use std::path::Path;

use serde::{Deserialize, Serialize};

pub trait RubricBackend {
    fn evaluate(&self, capture: &Path, rubric: &Rubric) -> Result<RubricVerdict, RubricError>;
}

#[derive(Debug, thiserror::Error)]
pub enum RubricError {
    #[error("{0}")]
    Failed(String),
    #[error("{message}")]
    RateLimited {
        message: String,
        retry_after_ms: Option<u64>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rubric {
    pub name: String,
    pub question: String,
    pub model: Option<String>,
    pub effort: Option<RubricEffort>,
    pub system_prompt: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RubricEffort(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RubricVerdictStatus {
    Pass,
    Fail,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RubricVerdict {
    pub verdict: RubricVerdictStatus,
    pub reason: String,
    #[serde(default)]
    pub anomalies: Vec<String>,
}

/// Uses the `visual-rubric` Codex ACP backend explicitly, never on default runs.
#[cfg(feature = "rubric")]
#[derive(Clone, Copy, Debug, Default)]
pub struct CodexAcpRubric;

#[cfg(feature = "rubric")]
impl RubricBackend for CodexAcpRubric {
    fn evaluate(&self, capture: &Path, rubric: &Rubric) -> Result<RubricVerdict, RubricError> {
        let png =
            png_for_backend(capture).map_err(|error| RubricError::Failed(error.to_string()))?;
        let verdict = visual_rubric::evaluate_image_rubric_with_options(
            png.path(),
            &rubric.question,
            visual_rubric::RubricOptions {
                model: rubric.model.clone(),
                effort: rubric.effort.as_ref().map(|effort| effort.0.clone().into()),
                system_prompt: rubric.system_prompt.clone(),
            },
        )
        .map_err(|error| match &error {
            visual_rubric::RubricError::Pool(visual_rubric::PoolError::RateLimited {
                retry_after,
            }) => RubricError::RateLimited {
                message: error.to_string(),
                retry_after_ms: retry_after
                    .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)),
            },
            _ => RubricError::Failed(error.to_string()),
        })?;
        let status = match verdict.verdict.as_str() {
            "pass" => RubricVerdictStatus::Pass,
            "fail" => RubricVerdictStatus::Fail,
            status => {
                return Err(RubricError::Failed(format!(
                    "unknown rubric verdict {status:?}"
                )));
            }
        };
        Ok(RubricVerdict {
            verdict: status,
            reason: verdict.reason,
            anomalies: verdict.anomalies,
        })
    }
}

#[cfg(feature = "rubric")]
fn png_for_backend(capture: &Path) -> image::ImageResult<tempfile::NamedTempFile> {
    let mut png = tempfile::Builder::new().suffix(".png").tempfile()?;
    image::open(capture)?.write_to(png.as_file_mut(), image::ImageFormat::Png)?;
    Ok(png)
}
