//! Reproducibility checks for the adapter Bevy actually selected.

use bevy::{prelude::App, render::renderer::RenderAdapterInfo};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const ALLOW_UNPINNED_ENV: &str = "HARBOR_VISUAL_TEST_ALLOW_UNPINNED";
pub const PROFILE_ENV: &str = "RS_HARBOR_GPU_PIN_PROFILE";
const BACKEND: &str = "RS_HARBOR_GPU_EXPECTED_BACKEND";
const VENDOR: &str = "RS_HARBOR_GPU_EXPECTED_VENDOR";
const DEVICE: &str = "RS_HARBOR_GPU_EXPECTED_DEVICE";
const DEVICE_TYPE: &str = "RS_HARBOR_GPU_EXPECTED_DEVICE_TYPE";
const NAME: &str = "RS_HARBOR_GPU_EXPECTED_NAME_CONTAINS";
const DRIVER: &str = "RS_HARBOR_GPU_EXPECTED_DRIVER_CONTAINS";
const DRIVER_INFO: &str = "RS_HARBOR_GPU_EXPECTED_DRIVER_INFO_CONTAINS";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuAdapterIdentity {
    pub name: String,
    pub vendor: u32,
    pub device: u32,
    pub device_type: String,
    pub driver: String,
    pub driver_info: String,
    pub backend: String,
}

impl From<&RenderAdapterInfo> for GpuAdapterIdentity {
    fn from(info: &RenderAdapterInfo) -> Self {
        Self {
            name: info.name.trim().into(),
            vendor: info.vendor,
            device: info.device,
            device_type: format!("{:?}", info.device_type),
            driver: info.driver.trim().into(),
            driver_info: info.driver_info.trim().into(),
            backend: format!("{:?}", info.backend),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidationDecision {
    Pinned,
    AllowedUnpinned,
}

#[derive(Debug, Error)]
pub enum GpuPinError {
    #[error("RenderAdapterInfo missing after renderer initialization")]
    MissingAdapter,
    #[error(
        "no GPU identity constraints set for profile {profile:?}; actual adapter: {actual:?}; set {ALLOW_UNPINNED_ENV}=1 only for local debugging"
    )]
    MissingPin {
        profile: String,
        actual: Box<GpuAdapterIdentity>,
    },
    #[error("invalid GPU pin {key}={value:?}: expected a decimal or 0x-prefixed hexadecimal u32")]
    InvalidExpected { key: &'static str, value: String },
    #[error("GPU pin mismatch for profile {profile:?}: {}", mismatches.join("; "))]
    Mismatch {
        profile: String,
        mismatches: Vec<String>,
    },
}

pub fn validate_app(app: &App) -> Result<(GpuAdapterIdentity, ValidationDecision), GpuPinError> {
    let actual = GpuAdapterIdentity::from(
        app.world()
            .get_resource::<RenderAdapterInfo>()
            .ok_or(GpuPinError::MissingAdapter)?,
    );
    let decision = validate_with_lookup(&actual, |key| std::env::var(key).ok())?;
    Ok((actual, decision))
}

pub fn validate_with_lookup(
    actual: &GpuAdapterIdentity,
    lookup: impl Fn(&'static str) -> Option<String>,
) -> Result<ValidationDecision, GpuPinError> {
    if lookup(ALLOW_UNPINNED_ENV).as_deref() == Some("1") {
        return Ok(ValidationDecision::AllowedUnpinned);
    }
    let get = |key| {
        lookup(key)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    let profile = get(PROFILE_ENV).unwrap_or_else(|| "<unnamed>".into());
    let mut constraints = 0;
    let mut mismatches = Vec::new();
    for (key, current) in [(VENDOR, actual.vendor), (DEVICE, actual.device)] {
        if let Some(value) = get(key) {
            constraints += 1;
            let expected = if let Some(hex) = value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
            {
                u32::from_str_radix(hex, 16)
            } else {
                value.parse()
            }
            .map_err(|_| GpuPinError::InvalidExpected { key, value })?;
            if expected != current {
                mismatches.push(format!(
                    "{key}: expected 0x{expected:x}, actual 0x{current:x}"
                ));
            }
        }
    }
    for (key, current, substring) in [
        (BACKEND, &actual.backend, false),
        (DEVICE_TYPE, &actual.device_type, false),
        (NAME, &actual.name, true),
        (DRIVER, &actual.driver, true),
        (DRIVER_INFO, &actual.driver_info, true),
    ] {
        if let Some(expected) = get(key) {
            constraints += 1;
            let matches = if substring {
                current.to_lowercase().contains(&expected.to_lowercase())
            } else {
                current.trim().eq_ignore_ascii_case(&expected)
            };
            if !matches {
                mismatches.push(format!("{key}: expected {expected:?}, actual {current:?}"));
            }
        }
    }
    if constraints == 0 {
        Err(GpuPinError::MissingPin {
            profile,
            actual: Box::new(actual.clone()),
        })
    } else if mismatches.is_empty() {
        Ok(ValidationDecision::Pinned)
    } else {
        Err(GpuPinError::Mismatch {
            profile,
            mismatches,
        })
    }
}
