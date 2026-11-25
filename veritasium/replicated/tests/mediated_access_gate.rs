//! Compile-time canaries for the forked Bevy `MediatedAccess` gate.
//!
//! If Bevy's diagnostics or the `#[derive(Replicated)]` expansion changes,
//! regenerate the stderr baselines with:
//!
//! `TRYBUILD=overwrite cargo test -p veritasium_replicated --test mediated_access_gate`

use std::{env, fs, path::PathBuf, process::Command};

#[test]
fn mediated_access_gate_rejects_bad_patterns() {
    const CHILD: &str = "HARBOR_BEVY_TRYBUILD_CHILD";
    if env::var_os(CHILD).is_none() {
        // trybuild normalizes registry paths but not Git checkout paths. Set
        // rustflags in a child so the process environment of the harness stays
        // untouched and snapshots do not depend on the local Cargo cache.
        let cargo_home = env::var_os("CARGO_HOME").map_or_else(
            || env::home_dir().expect("home directory").join(".cargo"),
            PathBuf::from,
        );
        let checkouts = cargo_home.join("git/checkouts");
        let mut flags = vec![
            "--cfg".to_owned(),
            "trybuild".to_owned(),
            "--verbose".to_owned(),
            "--diagnostic-width=140".to_owned(),
            "-Adead_code".to_owned(),
        ];
        for cache in fs::read_dir(checkouts).expect("Cargo Git checkouts") {
            let cache = cache.expect("Git cache entry");
            if !cache
                .file_name()
                .to_string_lossy()
                .starts_with("rs-veritasium-")
            {
                continue;
            }
            for checkout in fs::read_dir(cache.path()).expect("Veritasium checkouts") {
                let path = checkout.expect("Veritasium checkout").path();
                if path.is_dir() {
                    flags.push(format!(
                        "--remap-path-prefix={}=/veritasium",
                        path.display()
                    ));
                }
            }
        }
        let status = Command::new(env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "mediated_access_gate_rejects_bad_patterns",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("CARGO_ENCODED_RUSTFLAGS", flags.join("\x1f"))
            .status()
            .expect("start trybuild child");
        assert!(status.success(), "mediated-access fixtures failed");
        return;
    }

    let t = trybuild::TestCases::new();
    t.compile_fail("compile_fail/*.rs");
    t.pass("compile_pass/*.rs");
}
