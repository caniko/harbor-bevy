use super::*;

#[test]
fn deterministic_arguments() -> Result<()> {
    let options = Ktx2CookOptions::default();
    let root = std::env::current_dir()?;
    let output = root.join("stage/a.ktx2");
    let mut source = TextureSource {
        input: root.join("input.png"),
        output_name: "a.ktx2".into(),
        color_space: ColorSpace::Srgb,
        mipmaps: true,
    };
    let args = toktx_args(&source, &options, &output)?;
    assert_eq!(
        args[..args.len() - 2],
        [
            "--t2",
            "--uastc",
            "2",
            "--zcmp",
            "18",
            "--genmipmap",
            "--assign_oetf",
            "srgb",
            "--threads",
            "1",
        ]
    );
    assert_eq!(
        args[args.len() - 2..],
        [output.as_os_str(), source.input.as_os_str()]
    );
    source.color_space = ColorSpace::Linear;
    source.mipmaps = false;
    let options = Ktx2CookOptions {
        uastc_level: 1,
        zcmp_level: 9,
        ..options
    };
    let args = toktx_args(&source, &options, &output)?;
    assert_eq!(
        args[..args.len() - 2],
        [
            "--t2",
            "--uastc",
            "1",
            "--zcmp",
            "9",
            "--assign_oetf",
            "linear",
            "--threads",
            "1",
        ]
    );
    Ok(())
}

#[cfg(unix)]
fn fixture() -> Result<(tempfile::TempDir, Ktx2CookOptions, Vec<TextureSource>)> {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir()?;
    let toktx = dir.path().join("fake-toktx");
    fs::write(
        &toktx,
        "#!/bin/sh\nset -eu\nwhile [ \"$#\" -gt 2 ]; do shift; done\ncp -- \"$2\" \"$1\"\n",
    )?;
    fs::set_permissions(&toktx, fs::Permissions::from_mode(0o755))?;
    let input = dir.path().join("input.png");
    fs::write(&input, b"abc")?;
    let sources = ["b.ktx2", "a.ktx2"].map(|output_name| TextureSource {
        input: input.clone(),
        output_name: output_name.into(),
        color_space: ColorSpace::Srgb,
        mipmaps: false,
    });
    let options = Ktx2CookOptions {
        output_dir: dir.path().join("pack"),
        toktx,
        ..Ktx2CookOptions::default()
    };
    Ok((dir, options, sources.into()))
}

#[cfg(unix)]
#[test]
fn verifies_sorted_sums_and_reports_drift_without_repairing_it() -> Result<()> {
    let (_dir, mut options, sources) = fixture()?;
    cook_ktx2_pack(&sources, &options)?;
    let out = options.output_dir.clone();
    let hash = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    let sums = format!("{hash}  a.ktx2\n{hash}  b.ktx2\n");
    assert_eq!(fs::read_to_string(out.join("SHA256SUMS"))?, sums);
    options.check = true;
    cook_ktx2_pack(&sources, &options)?;

    for name in ["a.ktx2", "SHA256SUMS"] {
        let path = out.join(name);
        let original = fs::read(&path)?;
        fs::write(&path, b"bad")?;
        assert!(cook_ktx2_pack(&sources, &options).is_err());
        assert_eq!(fs::read(&path)?, b"bad");
        fs::remove_file(&path)?;
        assert!(cook_ktx2_pack(&sources, &options).is_err());
        assert!(!path.exists());
        fs::write(&path, original)?;
    }
    let extra = out.join("unexpected.ktx2");
    fs::write(&extra, b"extra")?;
    assert!(cook_ktx2_pack(&sources, &options).is_err());
    assert_eq!(fs::read(extra)?, b"extra");
    options.output_dir = out.join("../absent/pack");
    assert!(cook_ktx2_pack(&sources, &options).is_err());
    assert!(!out.with_file_name("absent").exists());
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn failed_cook_preserves_destination_and_success_replaces_pack() -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let (_dir, options, mut sources) = fixture()?;
    let out = &options.output_dir;
    cook_ktx2_pack(&sources, &options)?;
    let old_inode = fs::metadata(out)?.ino();
    let old_sums = fs::read(out.join("SHA256SUMS"))?;
    fs::write(&sources[1].input, b"new")?;
    sources[0].input.set_file_name("missing.png");
    assert!(cook_ktx2_pack(&sources, &options).is_err());
    assert_eq!(fs::metadata(out)?.ino(), old_inode);
    assert_eq!(fs::read(out.join("a.ktx2"))?, b"abc");
    assert_eq!(fs::read(out.join("SHA256SUMS"))?, old_sums);

    let unrelated = out.join("keep.txt");
    fs::write(&unrelated, b"keep")?;
    assert!(cook_ktx2_pack(&sources[1..], &options).is_err());
    assert_eq!(fs::read(&unrelated)?, b"keep");
    fs::remove_file(unrelated)?;
    cook_ktx2_pack(&sources[1..], &options)?;
    assert_ne!(fs::metadata(out)?.ino(), old_inode);
    assert_eq!(fs::read(out.join("a.ktx2"))?, b"new");
    assert!(!out.join("b.ktx2").exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn rejects_invalid_and_duplicate_names_before_touching_destination() -> Result<()> {
    let (_dir, options, mut sources) = fixture()?;
    for name in [
        "../a.ktx2",
        "a/b.ktx2",
        "a\\b.ktx2",
        "x\n.ktx2",
        "a.png",
        "a.ktx2",
    ] {
        sources[0].output_name = name.into();
        assert!(cook_ktx2_pack(&sources, &options).is_err(), "{name}");
        assert!(!options.output_dir.exists());
    }
    Ok(())
}
