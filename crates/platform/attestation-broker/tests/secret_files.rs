#![cfg(feature = "bin")]

#[test]
fn missing_configured_secret_file_fails_startup() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_attestation-broker"))
        .args([
            "--steam-app-id",
            "1",
            "--detritus-url",
            "http://127.0.0.1:4317",
        ])
        .env("ATTESTATION_PLAY_PACKAGE", "com.example.game")
        .env("ATTESTATION_PLAY_AGE_HASH_DOMAIN", "example:play-age:v1")
        .env("STEAM_WEB_API_KEY", "fallback-must-not-be-used")
        .env(
            "STEAM_WEB_API_KEY_FILE",
            "/nonexistent/attestation-broker-secret",
        )
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("secret file unreadable"));
}

#[test]
fn both_play_policy_flags_are_required_without_environment() {
    for (supplied, value, missing) in [
        (
            "--play-package",
            "com.example.game",
            "--play-age-hash-domain",
        ),
        (
            "--play-age-hash-domain",
            "example:play-age:v1",
            "--play-package",
        ),
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_attestation-broker"))
            .args([
                "--steam-app-id",
                "1",
                "--detritus-url",
                "http://127.0.0.1:4317",
                supplied,
                value,
            ])
            .env_remove("ATTESTATION_PLAY_PACKAGE")
            .env_remove("ATTESTATION_PLAY_AGE_HASH_DOMAIN")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2));
        assert!(
            stderr.contains("the following required arguments were not provided"),
            "{stderr}"
        );
        assert!(stderr.contains(missing), "{stderr}");
    }
}
