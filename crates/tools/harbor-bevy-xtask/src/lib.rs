//! Project-local task and policy SDK.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use serde::Deserialize;

pub mod denylist;
pub mod doctor;
pub mod license;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub app_name: String,
    pub binary_name: String,
    #[serde(default)]
    pub paths: ProjectPaths,
    #[serde(default)]
    pub denylist_patterns: Vec<String>,
    #[serde(default)]
    pub bevy: BevyConfig,
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub android_flavors: BTreeMap<String, toml::Table>,
    pub steam: Option<SteamConfig>,
    #[serde(default)]
    pub test_suites: BTreeMap<String, toml::Table>,
    #[serde(default)]
    pub gameplay: toml::Table,
    #[serde(default)]
    pub cluster: toml::Table,
}

impl ProjectConfig {
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("harbor-bevy.toml");
        let config: Self = read_toml(&path)?;
        ensure!(!config.app_name.trim().is_empty(), "app_name is empty");
        ensure!(
            !config.binary_name.trim().is_empty() && !config.binary_name.contains(['/', '\\']),
            "binary_name must be a binary name, not a path"
        );
        Ok(config)
    }
}

fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let contents = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    toml::from_str(&contents).with_context(|| format!("parse {}", path.display()))
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectPaths {
    /// Path to the workspace manifest, where Cargo reads `[patch.crates-io]`.
    pub cargo_manifest: PathBuf,
    pub license_classes: PathBuf,
    pub build: PathBuf,
    pub assets: PathBuf,
    pub dist: PathBuf,
}

impl Default for ProjectPaths {
    fn default() -> Self {
        Self {
            cargo_manifest: "Cargo.toml".into(),
            license_classes: "licensing/package-classes.toml".into(),
            build: "target".into(),
            assets: "assets".into(),
            dist: "dist".into(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BevyConfig {
    pub mode: BevyMode,
    pub package_list: Option<PathBuf>,
    pub veritasium_repo: Option<PathBuf>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BevyMode {
    #[default]
    Stock,
    #[serde(alias = "veritasium-fork")]
    Veritasium,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteamConfig {
    pub app_id: u32,
    #[serde(default)]
    pub depots: BTreeMap<String, u32>,
}

#[derive(Debug, Parser)]
pub struct KitCli {
    #[command(subcommand)]
    pub command: KitCommand,
}

#[derive(Debug, Subcommand)]
pub enum KitCommand {
    Doctor {
        #[command(subcommand)]
        command: DoctorCommand,
    },
    License,
    Denylist,
    #[command(flatten)]
    Project(ProjectCommand),
}

#[derive(Debug, Subcommand)]
pub enum DoctorCommand {
    Patches,
}

macro_rules! project_commands {
    ($($command:ident),+ $(,)?) => {
        #[derive(Debug, Subcommand)]
        pub enum ProjectCommand {
            $(
                $command {
                    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
                    args: Vec<OsString>,
                },
            )+
        }
    };
}

project_commands!(
    Build,
    Assets,
    Seal,
    Sign,
    Release,
    Steam,
    Suite,
    AndroidGate,
    Gameplay,
    Cluster
);

pub struct Project<'a> {
    pub root: &'a Path,
    pub config: &'a ProjectConfig,
}

/// Implement this for consumer-owned build, packaging, and release conventions.
pub trait ProjectHooks {
    fn run(&self, project: &Project<'_>, command: ProjectCommand) -> Result<()>;
}

impl KitCommand {
    pub fn run(self, root: &Path, hooks: &impl ProjectHooks) -> Result<()> {
        let root = root.canonicalize()?;
        let config = ProjectConfig::load(&root)?;
        let project = Project {
            root: &root,
            config: &config,
        };
        match self {
            Self::Doctor {
                command: DoctorCommand::Patches,
            } => doctor::run(&project),
            Self::License => license::run(&project),
            Self::Denylist => {
                let hits = denylist::scan(&root, &config.denylist_patterns)?;
                for hit in &hits {
                    eprintln!("{}:{}", hit.path.display(), hit.line);
                }
                ensure!(hits.is_empty(), "{} denylist matches", hits.len());
                Ok(())
            }
            Self::Project(command) => hooks.run(&project, command),
        }
    }
}

impl Project<'_> {
    /// Run the pinned Fragpipe CLI in-process. Its relative paths use the process CWD.
    pub fn gameplay(&self, args: impl IntoIterator<Item = OsString>) -> Result<()> {
        self.require_current_dir()?;
        fragpipe::run(fragpipe::Cli::try_parse_from(
            std::iter::once(OsString::from("fragpipe")).chain(args),
        )?)
    }

    /// Run the opt-in cluster CLI in-process. Its relative paths use the process CWD.
    #[cfg(feature = "cluster")]
    pub fn cluster(&self, args: impl IntoIterator<Item = OsString>) -> Result<()> {
        self.require_current_dir()?;
        steampipe::run(steampipe::Cli::try_parse_from(
            std::iter::once(OsString::from("cluster-ctl")).chain(args),
        )?)
    }

    fn require_current_dir(&self) -> Result<()> {
        ensure!(
            std::env::current_dir()?.canonicalize()? == self.root.canonicalize()?,
            "run this command from {}",
            self.root.display()
        );
        Ok(())
    }

    fn manifest(&self) -> PathBuf {
        self.root.join(&self.config.paths.cargo_manifest)
    }
}

fn cargo_metadata(manifest: &Path, no_deps: bool) -> Result<String> {
    let mut cmd = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cmd.args(["metadata", "--format-version", "1", "--manifest-path"])
        .arg(manifest)
        .current_dir(manifest.parent().context("Cargo manifest has no parent")?);
    if no_deps {
        cmd.arg("--no-deps");
    } else {
        cmd.args(["--locked", "--all-features"]);
    }
    let output = cmd.output().context("execute cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).context("cargo metadata returned non-UTF-8 output")
}

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    #[serde(default)]
    workspace_members: Vec<String>,
    resolve: Option<Resolve>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    version: String,
    id: String,
    source: Option<String>,
    manifest_path: Option<PathBuf>,
    license: Option<String>,
    publish: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}

#[derive(Deserialize)]
struct Node {
    id: String,
    #[serde(default)]
    dependencies: Vec<String>,
}

#[cfg(test)]
mod tests;
