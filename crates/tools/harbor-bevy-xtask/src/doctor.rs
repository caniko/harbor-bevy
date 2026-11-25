//! Verify the selected engine and replication crate identities, not just patch declarations.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use crate::{BevyMode, Metadata, Package, Project, cargo_metadata};

pub const ENGINE_LIST: &str = include_str!("../bevy-0.20.1-packages.txt");
const ENGINE_VERSION: &str = "0.20.1";
const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";
const REPLICATION: [&str; 3] = [
    "veritasium_wire",
    "veritasium_replicated",
    "veritasium_replicated_macros",
];

pub fn run(project: &Project<'_>) -> Result<()> {
    let cfg = &project.config.bevy;
    ensure!(
        cfg.package_list.is_none() || cfg.veritasium_repo.is_none(),
        "choose package_list or veritasium_repo, not both"
    );
    let names = match (&cfg.package_list, &cfg.veritasium_repo) {
        (Some(path), _) => fs::read_to_string(project.root.join(path))
            .with_context(|| format!("read engine package list {}", path.display()))?,
        (_, Some(repo)) => {
            let metadata: Metadata = serde_json::from_str(&cargo_metadata(
                &project.root.join(repo).join("Cargo.toml"),
                true,
            )?)?;
            let expected = ENGINE_LIST.lines().collect::<BTreeSet<_>>();
            metadata
                .packages
                .into_iter()
                .filter(|pkg| {
                    pkg.version == ENGINE_VERSION
                        && expected.contains(pkg.name.as_str())
                        && metadata.workspace_members.contains(&pkg.id)
                })
                .map(|pkg| pkg.name + "\n")
                .collect()
        }
        _ => ENGINE_LIST.to_owned(),
    };
    let manifest = project.manifest();
    let content =
        fs::read_to_string(&manifest).with_context(|| format!("read {}", manifest.display()))?;
    manifest_patches(cfg.mode, &names, &content)?;
    validate(
        manifest.parent().context("Cargo manifest has no parent")?,
        cfg.mode,
        &names,
        &content,
        &cargo_metadata(&manifest, false)?,
    )
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    patch: BTreeMap<String, BTreeMap<String, toml::Value>>,
}

#[derive(Deserialize)]
struct Patch {
    #[serde(default)]
    package: Option<String>,
    path: Option<PathBuf>,
    git: Option<String>,
    rev: Option<String>,
}

fn manifest_patches<'a>(
    mode: BevyMode,
    package_list: &'a str,
    manifest: &str,
) -> Result<(BTreeSet<&'a str>, BTreeMap<String, Patch>)> {
    let engine: BTreeSet<_> = package_list
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    let expected: BTreeSet<_> = ENGINE_LIST.lines().collect();
    let missing = expected.difference(&engine).copied().collect::<Vec<_>>();
    let unexpected = engine.difference(&expected).copied().collect::<Vec<_>>();
    ensure!(
        missing.is_empty() && unexpected.is_empty(),
        "engine package list mismatch: missing {missing:?}; unexpected {unexpected:?}"
    );
    let manifest: Manifest = toml::from_str(manifest)?;
    let mut patches = BTreeMap::new();
    for (key, value) in manifest.patch.get("crates-io").into_iter().flatten() {
        let name = value
            .get("package")
            .and_then(toml::Value::as_str)
            .unwrap_or(key);
        if !engine.contains(name) {
            continue;
        }
        ensure!(
            mode == BevyMode::Veritasium,
            "stock mode patches engine package {name}"
        );
        let patch: Patch = value
            .clone()
            .try_into()
            .with_context(|| format!("invalid patch for {name}"))?;
        ensure!(
            patch.path.is_some() ^ patch.git.is_some(),
            "patch for {name} needs exactly one path or git source"
        );
        ensure!(
            patches
                .insert(patch.package.clone().unwrap_or_else(|| key.clone()), patch)
                .is_none(),
            "multiple patches for {name}"
        );
    }
    if mode == BevyMode::Veritasium {
        let missing: Vec<_> = engine
            .iter()
            .filter(|name| !patches.contains_key(**name))
            .copied()
            .collect();
        ensure!(
            missing.is_empty(),
            "missing [patch.crates-io] entries: {}",
            missing.join(", ")
        );
    }
    Ok((engine, patches))
}

/// Validate a manifest and full format-version-1 Cargo metadata for the same workspace.
pub fn validate(
    manifest_dir: &Path,
    mode: BevyMode,
    package_list: &str,
    manifest: &str,
    metadata: &str,
) -> Result<()> {
    let (engine, patches) = manifest_patches(mode, package_list, manifest)?;
    let metadata: Metadata = serde_json::from_str(metadata)?;
    let resolved: BTreeSet<_> = metadata
        .resolve
        .context("metadata has no resolved dependency graph")?
        .nodes
        .into_iter()
        .map(|node| node.id)
        .collect();
    let mut seen = BTreeMap::new();
    let mut engine_present = false;
    for pkg in metadata
        .packages
        .iter()
        .filter(|pkg| resolved.contains(&pkg.id))
    {
        let is_engine = engine.contains(pkg.name.as_str());
        if !is_engine && !REPLICATION.contains(&pkg.name.as_str()) {
            continue;
        }
        if let Some(other) = seen.insert(&pkg.name, &pkg.id) {
            ensure!(
                other == &pkg.id,
                "multiple identities for {}: {other}, {}",
                pkg.name,
                pkg.id
            );
        }
        if is_engine {
            engine_present = true;
            ensure!(
                pkg.version == ENGINE_VERSION,
                "{} resolves to {} instead of {ENGINE_VERSION}",
                pkg.name,
                pkg.version
            );
            let matches = match mode {
                BevyMode::Stock => pkg.source.as_deref() == Some(CRATES_IO),
                BevyMode::Veritasium => matches_patch(manifest_dir, pkg, &patches[&pkg.name])?,
            };
            ensure!(
                matches,
                "{} resolved from an unexpected source: {}",
                pkg.name,
                pkg.id
            );
        }
    }
    ensure!(
        engine_present,
        "metadata contains no resolved Bevy engine crates"
    );
    Ok(())
}

fn matches_patch(dir: &Path, pkg: &Package, patch: &Patch) -> Result<bool> {
    if let Some(path) = &patch.path {
        let expected = dir
            .join(path)
            .join("Cargo.toml")
            .canonicalize()
            .with_context(|| format!("resolve patch path for {}", pkg.name))?;
        return Ok(pkg.source.is_none()
            && pkg
                .manifest_path
                .as_ref()
                .and_then(|p| p.canonicalize().ok())
                == Some(expected));
    }
    let Some(source) = pkg.source.as_deref().and_then(|s| s.strip_prefix("git+")) else {
        return Ok(false);
    };
    let Some((reference, commit)) = source.rsplit_once('#') else {
        return Ok(false);
    };
    let url = reference.split_once('?').map_or(reference, |(url, _)| url);
    let normalize = |s: &str| s.trim_end_matches('/').trim_end_matches(".git").to_owned();
    Ok(patch
        .git
        .as_deref()
        .is_some_and(|git| normalize(git) == normalize(url))
        && patch.rev.as_ref().is_none_or(|rev| {
            commit.starts_with(rev)
                || reference.split_once('?').is_some_and(|(_, q)| {
                    q.split('&')
                        .any(|part| part.strip_prefix("rev=") == Some(rev))
                })
        }))
}
