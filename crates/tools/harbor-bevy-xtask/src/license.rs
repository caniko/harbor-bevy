//! Check Cargo workspace classifications and publishable package boundaries.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use crate::{Metadata, Project, cargo_metadata};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LicenseClass {
    MitApache,
    Proprietary,
    ThirdParty,
}

#[derive(Deserialize)]
struct Classes {
    package: Vec<Classification>,
}

#[derive(Deserialize)]
struct Classification {
    name: String,
    path: PathBuf,
    class: LicenseClass,
    publish: bool,
}

pub fn run(project: &Project<'_>) -> Result<()> {
    let path = project.root.join(&project.config.paths.license_classes);
    validate(
        project.root,
        &fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?,
        &cargo_metadata(&project.manifest(), false)?,
    )
}

/// Compare package classes with Cargo metadata and reject transitive proprietary
/// dependencies of publishable, dual-licensed workspace packages.
pub fn validate(root: &Path, policy: &str, metadata: &str) -> Result<()> {
    let root = root.canonicalize()?;
    let metadata: Metadata = serde_json::from_str(metadata)?;
    let classes: Classes = toml::from_str(policy)?;
    let mut by_name = HashMap::new();
    for class in &classes.package {
        ensure!(
            by_name.insert(&class.name, class).is_none(),
            "duplicate classification for {}",
            class.name
        );
    }
    let members: HashSet<_> = metadata.workspace_members.iter().collect();
    ensure!(
        !members.is_empty(),
        "metadata contains no workspace members"
    );
    let mut by_id = HashMap::new();
    let mut matched = HashSet::new();
    for member in metadata.packages.iter().filter(|p| members.contains(&p.id)) {
        let class = by_name
            .get(&member.name)
            .with_context(|| format!("unclassified package: {}", member.name))?;
        ensure!(
            matched.insert(&class.name),
            "duplicate workspace name {}",
            class.name
        );
        let actual = member
            .manifest_path
            .as_ref()
            .context("package has no manifest_path")?
            .canonicalize()?;
        let expected = root
            .join(&class.path)
            .join("Cargo.toml")
            .canonicalize()
            .with_context(|| format!("resolve classification path for {}", class.name))?;
        ensure!(
            expected.starts_with(&root) && actual == expected,
            "manifest path mismatch for {}",
            class.name
        );
        let publish = member
            .publish
            .as_ref()
            .is_none_or(|registries| !registries.is_empty());
        ensure!(
            publish == class.publish,
            "publish policy mismatch for {}",
            class.name
        );
        match class.class {
            LicenseClass::MitApache => ensure!(
                matches!(
                    member.license.as_deref(),
                    Some("MIT OR Apache-2.0" | "Apache-2.0 OR MIT")
                ),
                "{} must declare MIT OR Apache-2.0",
                class.name
            ),
            LicenseClass::Proprietary => {
                ensure!(!class.publish, "{} cannot be published", class.name)
            }
            LicenseClass::ThirdParty => ensure!(
                member
                    .license
                    .as_ref()
                    .is_some_and(|license| !license.trim().is_empty()),
                "{} must declare its third-party license",
                class.name
            ),
        }
        by_id.insert(member.id.as_str(), *class);
    }
    for name in by_name.keys() {
        ensure!(
            matched.contains(name),
            "classified package not in workspace: {name}"
        );
    }
    ensure!(
        by_id.len() == members.len(),
        "metadata is missing workspace packages"
    );
    let graph: HashMap<_, _> = metadata
        .resolve
        .as_ref()
        .context("metadata has no resolved dependency graph")?
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), &node.dependencies))
        .collect();
    for (&id, class) in &by_id {
        if class.class != LicenseClass::MitApache || !class.publish {
            continue;
        }
        let mut seen = HashSet::new();
        let mut stack = vec![id];
        while let Some(dependency) = stack.pop() {
            if !seen.insert(dependency) {
                continue;
            }
            if let Some(dep) = by_id.get(dependency) {
                ensure!(
                    dep.class != LicenseClass::Proprietary,
                    "proprietary package {} flows into publishable {}",
                    dep.name,
                    class.name
                );
            }
            let deps = graph
                .get(dependency)
                .with_context(|| format!("missing dependency node {dependency}"))?;
            stack.extend(deps.iter().map(String::as_str));
        }
    }
    Ok(())
}
