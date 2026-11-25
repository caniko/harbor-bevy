use certification_core::{PackageLayout, PackageRole, Platform};

#[test]
fn binary_name_controls_desktop_paths_and_no_depot_is_implicit() {
    let layout = PackageLayout::for_binary("demo").unwrap();
    for (platform, binary, symbols) in [
        (
            Platform::LinuxSteam,
            "dist/linux/demo",
            "dist/symbols/linux/demo.debug",
        ),
        (
            Platform::WindowsSteam,
            "dist/windows/demo.exe",
            "dist/symbols/windows/demo.pdb",
        ),
        (
            Platform::MacosSteam,
            "dist/macos/demo",
            "dist/symbols/macos/demo.dSYM",
        ),
    ] {
        let files = layout.required_files(platform);
        assert!(files.contains(&(binary.into(), PackageRole::Binary)));
        assert!(files.contains(&(symbols.into(), PackageRole::Symbols)));
    }
    assert_eq!(layout.steam_upload_instructions(), ["steam/app_build.vdf"]);
    assert!(PackageLayout::for_binary("../demo").is_err());
    assert!(PackageLayout::for_binary("a\\b").is_err());
}

#[test]
fn target_and_depots_can_be_configured_without_affecting_other_targets() {
    let layout = PackageLayout::for_binary("demo")
        .unwrap()
        .with_target_files(
            Platform::MacosSteam,
            vec![
                (
                    "mac/Demo.app/Contents/MacOS/demo".into(),
                    PackageRole::Binary,
                ),
                (
                    "mac/Demo.app/Contents/MacOS/libsteam_api.dylib".into(),
                    PackageRole::NativeLibrary,
                ),
                ("mac/Demo.dSYM".into(), PackageRole::Symbols),
            ],
        )
        .with_steam_depots([9001, 9002]);
    assert_eq!(
        layout.required_files(Platform::MacosSteam)[0].0,
        "mac/Demo.app/Contents/MacOS/demo"
    );
    assert_eq!(
        layout.required_files(Platform::LinuxSteam)[0].0,
        "dist/linux/demo"
    );
    assert_eq!(
        layout.steam_upload_instructions(),
        [
            "steam/app_build.vdf",
            "steam/depot_9001.vdf",
            "steam/depot_9002.vdf"
        ]
    );
}
