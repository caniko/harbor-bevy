//! Feature-gated re-exports for Harbor's Bevy plugins and platform utilities.
//!
//! `core` is enabled by default. `full` enables all Stock integrations;
//! `veritasium-observers` is separate and requires a consumer-supplied Bevy patch.

#[cfg(feature = "core")]
pub use harbor_bevy_core::{self as core, HarborCorePlugin};
#[cfg(feature = "diagnostics")]
pub use harbor_bevy_diagnostics as diagnostics;
#[cfg(feature = "telemetry")]
pub use harbor_bevy_telemetry as telemetry;
#[cfg(feature = "ui")]
pub use harbor_bevy_ui::{self as ui, HarborUiPlugin};
#[cfg(feature = "thespan")]
pub use thespan_bevy as thespan;

/// Bevy-free platform, policy, integrity, and telemetry libraries.
#[cfg(feature = "platform")]
pub mod platform {
    pub use certification_core;
    pub use diagnostics_policy;
    pub use http_client;
    pub use localization_core;
    pub use p2p_integrity;
    pub use parental_controls;
    pub use platform_attestation;
    pub use release_metadata;
    pub use telemetry_client;
    pub use telemetry_protocol;
    pub use trust_safety;
}
