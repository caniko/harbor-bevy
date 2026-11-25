use release_metadata::{
    AndroidVersionCodePolicy, ArtifactMeta, IncompleteManifest, PublicUrlError, PublicUrls,
    ReleaseChannel, ReleaseVersion, VersionError, is_release_application_id,
};

#[test]
fn version_helpers_use_caller_supplied_version() {
    let value = String::from("1.2.3");
    let v = version(&value);
    assert_eq!(v.semver, "1.2.3");
    assert_eq!(v.visible_text(), "1.2.3");
    assert_eq!(v.android_version_name(), "1.2.3");
    assert_eq!(
        v.android_version_code(AndroidVersionCodePolicy).unwrap(),
        1_020_003
    );
}

#[test]
fn expected_app_id_is_explicit_and_cannot_be_empty() {
    assert!(is_release_application_id(
        "com.example.game",
        "com.example.game"
    ));
    assert!(!is_release_application_id(
        "com.example.game.test",
        "com.example.game"
    ));
    assert!(is_release_application_id(
        "com.example.other",
        "com.example.other"
    ));
    assert!(!is_release_application_id("", ""));
}

#[test]
fn invalid_semver_fails_version_code() {
    let v = ReleaseVersion {
        semver: "0.2.0-ea",
        channel: ReleaseChannel::EarlyAccess,
    };
    assert_eq!(
        v.android_version_code(AndroidVersionCodePolicy),
        Err(VersionError::InvalidSemver)
    );
}

fn version(semver: &str) -> ReleaseVersion<'_> {
    ReleaseVersion {
        semver,
        channel: ReleaseChannel::EarlyAccess,
    }
}

#[test]
fn leading_zeroes_are_invalid_semver() {
    for semver in ["01.2.0", "0.02.0", "0.2.00", "00.2.0"] {
        assert_eq!(
            version(semver).android_version_code(AndroidVersionCodePolicy),
            Err(VersionError::InvalidSemver),
            "{semver}"
        );
    }
}

#[test]
fn version_code_rejects_patch_collision_and_stays_monotonic() {
    assert_eq!(
        version("0.2.10000").android_version_code(AndroidVersionCodePolicy),
        Err(VersionError::OutOfRange)
    );
    assert_eq!(
        AndroidVersionCodePolicy.encode(0, 2, 10_000),
        Err(VersionError::OutOfRange)
    );
    let v100 = version("0.1.9999")
        .android_version_code(AndroidVersionCodePolicy)
        .unwrap();
    let v200 = version("0.2.0")
        .android_version_code(AndroidVersionCodePolicy)
        .unwrap();
    let v201 = version("0.2.1")
        .android_version_code(AndroidVersionCodePolicy)
        .unwrap();
    assert_eq!(v200, 20_000);
    assert!(v100 < v200 && v200 < v201);
    assert_eq!(
        version("0.100.0").android_version_code(AndroidVersionCodePolicy),
        Err(VersionError::OutOfRange)
    );
    assert_eq!(
        AndroidVersionCodePolicy.encode(AndroidVersionCodePolicy::MAX_MAJOR + 1, 0, 0),
        Err(VersionError::OutOfRange)
    );
    AndroidVersionCodePolicy
        .encode(
            AndroidVersionCodePolicy::MAX_MAJOR,
            AndroidVersionCodePolicy::MAX_MINOR,
            AndroidVersionCodePolicy::MAX_PATCH,
        )
        .unwrap();
}

#[test]
fn public_urls_are_derived_from_the_caller_supplied_base() {
    let urls = PublicUrls::from_base("https://legal.test/").unwrap();
    assert_eq!(urls.base, "https://legal.test");
    assert_eq!(urls.privacy, "https://legal.test/privacy");
    assert_eq!(urls.eula, "https://legal.test/eula");
    assert_eq!(urls.support, "https://legal.test/support");
    assert_eq!(urls.broker, "https://legal.test/broker");
    assert_eq!(urls.diagnostics, "https://legal.test/diagnostics");
    assert_eq!(urls.deletion, "https://legal.test/diagnostics#delete-data");
    let nested = PublicUrls::from_base("https://legal.test/game").unwrap();
    assert_eq!(nested.support, "https://legal.test/game/support");
}

#[test]
fn placeholder_and_missing_urls_fail() {
    assert_eq!(
        PublicUrls::from_optional(None),
        Err(PublicUrlError::MissingPublicUrl)
    );
    assert_eq!(
        PublicUrls::from_base(""),
        Err(PublicUrlError::MissingPublicUrl)
    );
    assert_eq!(
        PublicUrls::from_base("http://legal.test"),
        Err(PublicUrlError::NotHttps)
    );
    assert_eq!(
        PublicUrls::from_base("https://"),
        Err(PublicUrlError::Malformed)
    );
    for url in [
        "https://localhost/legal",
        "https://example/legal",
        "https://example.com/privacy",
        "https://telemetry.example.com/api/v1/telemetry/batch",
        "http://localhost:8080",
        "https://legal.test/?q=1",
        "https://legal.test/#frag",
        "https://user:pass@legal.test/",
        "https://127.0.0.1/legal",
        "https://[::1]/legal",
        "https://[::ffff:127.0.0.1]/legal",
        "not a url",
    ] {
        assert!(
            PublicUrls::from_base(url).is_err(),
            "{url} must fail publication gate"
        );
    }
}

#[test]
fn userinfo_cannot_bypass_localhost_gate() {
    assert_eq!(
        PublicUrls::from_base("https://user:pass@localhost/legal"),
        Err(PublicUrlError::Credentials)
    );
    assert_eq!(
        PublicUrls::from_base("https://localhost:443@legal.test/"),
        Err(PublicUrlError::Credentials)
    );
    assert_eq!(
        PublicUrls::from_base("https://u:p@127.0.0.1/"),
        Err(PublicUrlError::Credentials)
    );
}

#[test]
fn unsigned_or_empty_artifact_fails_manifest() {
    let complete = ArtifactMeta {
        checksum_sha256: "abc".into(),
        source_sha: "def".into(),
        profile: "release".into(),
        platform: "linux".into(),
        signed: true,
    };
    complete.manifest_complete(true).unwrap();

    let mut unsigned = complete.clone();
    unsigned.signed = false;
    assert_eq!(unsigned.manifest_complete(true), Err(IncompleteManifest));
    unsigned.manifest_complete(false).unwrap();

    let mut empty = complete;
    empty.source_sha.clear();
    assert_eq!(empty.manifest_complete(true), Err(IncompleteManifest));
}
