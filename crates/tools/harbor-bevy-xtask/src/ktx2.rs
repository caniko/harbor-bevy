//! Deterministic KTX2 pack cooking.

use std::{
    ffi::OsString,
    fmt::Write as _,
    fs::{self, File},
    io::{self, BufRead, BufReader},
    path::{self, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

const SUMS: &str = "SHA256SUMS";

/// Transfer function to assign to the source samples, without converting them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    Srgb,
    Linear,
}

#[derive(Debug, Clone)]
pub struct TextureSource {
    pub input: PathBuf,
    /// A flat filename ending in `.ktx2`.
    pub output_name: String,
    pub color_space: ColorSpace,
    pub mipmaps: bool,
}

#[derive(Debug, Clone)]
pub struct Ktx2CookOptions {
    /// A dedicated directory owned by the pack cooker.
    pub output_dir: PathBuf,
    pub check: bool,
    /// A path to `toktx`, or a binary name to find on `PATH`.
    pub toktx: PathBuf,
    pub uastc_level: u8,
    pub zcmp_level: u8,
}

impl Default for Ktx2CookOptions {
    fn default() -> Self {
        Self {
            output_dir: PathBuf::new(),
            check: false,
            toktx: "toktx".into(),
            uastc_level: 2,
            zcmp_level: 18,
        }
    }
}

fn toktx_args(
    source: &TextureSource,
    options: &Ktx2CookOptions,
    output: &Path,
) -> Result<Vec<OsString>> {
    let mut args = vec![
        "--t2".into(),
        "--uastc".into(),
        options.uastc_level.to_string().into(),
        "--zcmp".into(),
        options.zcmp_level.to_string().into(),
    ];
    if source.mipmaps {
        args.push("--genmipmap".into());
    }
    let oetf = match source.color_space {
        ColorSpace::Srgb => "srgb",
        ColorSpace::Linear => "linear",
    };
    args.extend(["--assign_oetf", oetf, "--threads", "1"].map(OsString::from));
    args.push(output.as_os_str().into());
    args.push(path::absolute(&source.input)?.into_os_string());
    Ok(args)
}

fn valid_name(name: &str) -> bool {
    name.strip_suffix(".ktx2").is_some_and(|stem| {
        !stem.is_empty() && !name.contains(['/', '\\', ':']) && !name.chars().any(char::is_control)
    })
}

/// Cook and atomically publish a pack, or compare it byte-for-byte in check mode.
///
/// Publishing requires directory synchronization support. Replacing an existing
/// pack also requires GNU `mv --exchange` and a filesystem that supports it.
/// The destination may contain only regular `.ktx2` files and `SHA256SUMS`.
/// Keep source images outside that directory.
pub fn cook_ktx2_pack(sources: &[TextureSource], options: &Ktx2CookOptions) -> Result<()> {
    let mut sources: Vec<_> = sources.iter().collect();
    sources.sort_unstable_by(|a, b| a.output_name.cmp(&b.output_name));
    for source in &sources {
        ensure!(
            valid_name(&source.output_name),
            "invalid KTX2 output name: {:?}",
            source.output_name
        );
    }
    for pair in sources.windows(2) {
        ensure!(
            pair[0].output_name != pair[1].output_name,
            "duplicate KTX2 output name: {}",
            pair[0].output_name
        );
    }

    let name = options
        .output_dir
        .file_name()
        .context("expected a named pack directory")?;
    let out = path::absolute(&options.output_dir)?;
    let parent = out.parent().context("expected a pack parent directory")?;
    // Strip trailing separators so symlink_metadata inspects the final component.
    let out = parent.join(name);
    if !options.check {
        fs::create_dir_all(parent)?;
    }
    let stage = tempfile::tempdir_in(parent).context("create sibling staging directory")?;
    let staged_pack = stage.path().join("pack");
    fs::create_dir(&staged_pack)?;
    let mut sums = String::new();
    for source in &sources {
        let cooked = staged_pack.join(&source.output_name);
        ensure!(
            !cooked.try_exists()?,
            "colliding output path: {}",
            cooked.display()
        );
        let run = Command::new(&options.toktx)
            .env_remove("TOKTX_OPTIONS")
            .args(toktx_args(source, options, &cooked)?)
            .output()
            .with_context(|| format!("run {}", options.toktx.display()))?;
        ensure!(
            run.status.success(),
            "toktx failed for {} ({}): {}",
            source.input.display(),
            run.status,
            String::from_utf8_lossy(&run.stderr).trim()
        );
        ensure!(
            fs::symlink_metadata(&cooked)?.is_file(),
            "toktx did not produce a regular file: {}",
            cooked.display()
        );
        let mut file = BufReader::new(File::open(&cooked)?);
        let mut hash = Sha256::new();
        loop {
            let bytes = file.fill_buf()?;
            if bytes.is_empty() {
                break;
            }
            hash.update(bytes);
            let len = bytes.len();
            file.consume(len);
        }
        file.get_ref().sync_all()?;
        writeln!(
            sums,
            "{}  {}",
            hex::encode(hash.finalize()),
            source.output_name
        )?;
    }
    let sums_path = staged_pack.join(SUMS);
    fs::write(&sums_path, sums)?;
    File::open(sums_path)?.sync_all()?;

    let existing = match fs::symlink_metadata(&out) {
        Ok(meta) => {
            ensure!(meta.is_dir(), "expected pack directory: {}", out.display());
            for entry in fs::read_dir(&out)? {
                let entry = entry?;
                ensure!(
                    entry.file_type()?.is_file()
                        && entry.file_name().to_str().is_some_and(|name| {
                            name == SUMS
                                || if options.check {
                                    sources
                                        .binary_search_by(|source| {
                                            source.output_name.as_str().cmp(name)
                                        })
                                        .is_ok()
                                } else {
                                    valid_name(name)
                                }
                        }),
                    "unexpected pack member: {}",
                    entry.path().display()
                );
            }
            Some(meta)
        }
        Err(err) if !options.check && err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(err).with_context(|| format!("inspect pack {}", out.display())),
    };
    if options.check {
        let names = sources.iter().map(|source| source.output_name.as_str());
        for name in names.chain([SUMS]) {
            let installed = out.join(name);
            ensure!(
                fs::read(staged_pack.join(name))?
                    == fs::read(&installed)
                        .with_context(|| format!("read {}", installed.display()))?,
                "stale cooked member: {}",
                installed.display()
            );
        }
    } else {
        let staged_dir = File::open(&staged_pack)?;
        if let Some(meta) = &existing {
            fs::set_permissions(&staged_pack, meta.permissions())?;
        }
        staged_dir.sync_all()?;
        File::open(stage.path())?.sync_all()?;
        if existing.is_some() {
            let swap = Command::new("mv")
                .args(["--exchange", "--no-copy", "--no-target-directory", "--"])
                .arg(&staged_pack)
                .arg(&out)
                .output()
                .context("run GNU mv to exchange pack directories")?;
            ensure!(
                swap.status.success(),
                "atomic pack exchange failed ({}): {}",
                swap.status,
                String::from_utf8_lossy(&swap.stderr).trim()
            );
        } else {
            fs::rename(&staged_pack, &out)?;
        }
        File::open(stage.path())?.sync_all()?;
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
