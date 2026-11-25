use std::path::{Path, PathBuf};

/// Application-supplied names used to namespace local data.
///
/// Supply trusted, single-component directory names, not user input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppIdentity {
    pub organization: String,
    pub application: String,
}

impl AppIdentity {
    pub fn new(organization: impl Into<String>, application: impl Into<String>) -> Self {
        Self {
            organization: organization.into(),
            application: application.into(),
        }
    }

    /// Use the platform's local data directory, falling back to the working directory.
    pub fn data_dir(&self) -> PathBuf {
        dirs::data_local_dir()
            .as_deref()
            .unwrap_or_else(|| Path::new("."))
            .join(&self.organization)
            .join(&self.application)
    }
}
