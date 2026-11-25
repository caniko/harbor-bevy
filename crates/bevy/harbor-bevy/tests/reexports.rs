#[cfg(any(
    feature = "core",
    feature = "thespan",
    feature = "ui",
    feature = "diagnostics",
    feature = "telemetry",
    feature = "platform",
))]
fn assert_same_type<A: 'static, B: 'static>() {
    use std::any::TypeId;

    assert_eq!(TypeId::of::<A>(), TypeId::of::<B>());
}

#[cfg(feature = "core")]
#[test]
fn core_reexports_original_types() {
    assert_same_type::<harbor_bevy::core::AppIdentity, harbor_bevy_core::AppIdentity>();
}

#[cfg(feature = "thespan")]
#[test]
fn thespan_reexports_original_types() {
    assert_same_type::<harbor_bevy::thespan::ThespanBevyPlugin, thespan_bevy::ThespanBevyPlugin>();
}

#[cfg(feature = "ui")]
#[test]
fn ui_reexports_original_types() {
    assert_same_type::<
        harbor_bevy::ui::viewport::ViewportPlugin,
        harbor_bevy_ui::viewport::ViewportPlugin,
    >();
}

#[cfg(feature = "diagnostics")]
#[test]
fn diagnostics_reexports_bevy_plugin() {
    assert_same_type::<
        harbor_bevy::diagnostics::plugin::DiagnosticsPlugin,
        harbor_bevy_diagnostics::plugin::DiagnosticsPlugin,
    >();
}

#[cfg(feature = "telemetry")]
#[test]
fn telemetry_reexports_original_types() {
    assert_same_type::<
        harbor_bevy::telemetry::TelemetryConfig,
        harbor_bevy_telemetry::TelemetryConfig,
    >();
}

#[cfg(feature = "platform")]
#[test]
fn platform_reexports_all_crates() {
    use harbor_bevy::platform as p;

    assert_same_type::<p::certification_core::Platform, certification_core::Platform>();
    assert_same_type::<p::diagnostics_policy::Consent, diagnostics_policy::Consent>();
    let _: fn(&(dyn std::error::Error + 'static)) -> String = p::http_client::error_chain;
    assert_same_type::<p::localization_core::LocaleId, localization_core::LocaleId>();
    assert_same_type::<p::p2p_integrity::SignedAction<()>, p2p_integrity::SignedAction<()>>();
    assert_same_type::<p::parental_controls::AgeBand, parental_controls::AgeBand>();
    assert_same_type::<p::platform_attestation::Platform, platform_attestation::Platform>();
    assert_same_type::<p::release_metadata::ReleaseChannel, release_metadata::ReleaseChannel>();
    assert_same_type::<p::telemetry_client::TelemetryClient, telemetry_client::TelemetryClient>();
    assert_same_type::<p::telemetry_protocol::Timestamp, telemetry_protocol::Timestamp>();
    assert_same_type::<p::trust_safety::ReportReason, trust_safety::ReportReason>();
}
