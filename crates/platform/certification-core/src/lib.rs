//! Bevy-independent provenance, package identity, and fail-closed certification.

use std::{collections::HashMap, path::Path};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceIdentity {
    pub commit: String,
    pub workspace_digest: String,
    pub dirty: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanonicalRef {
    ReleaseTag,
    ApprovedHead,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Pass,
    Fail,
    Blocked,
    Skipped,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Platform {
    LinuxSteam,
    WindowsSteam,
    MacosSteam,
    AndroidApk,
    AndroidAab,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageRole {
    Package,
    Binary,
    NativeLibrary,
    Assets,
    Symbols,
    Manifest,
    SigningEvidence,
    UploadInstructions,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageEntry {
    pub path: String,
    pub digest: String,
    pub size: u64,
    pub kind: EntryKind,
    pub executable: bool,
    pub symlink_target: Option<String>,
    pub arch: Option<String>,
    pub role: PackageRole,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildEnvironmentIdentity {
    pub target: String,
    pub profile: String,
    pub features: String,
    pub rustc: String,
    pub cargo: String,
    pub flake_lock_digest: String,
    pub toolchain_digest: String,
    pub sdk: String,
    pub hermetic: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnsignedPackageManifest {
    pub source: SourceIdentity,
    pub environment: BuildEnvironmentIdentity,
    pub platform: Platform,
    pub feature_set: String,
    pub cargo_lock_digest: String,
    pub entries: Vec<PackageEntry>,
    pub digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DistributionManifest {
    pub platform: Platform,
    pub unsigned_digest: String,
    pub package_digest: String,
    pub signed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseIndex {
    pub source: SourceIdentity,
    pub packages: Vec<(Platform, String)>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExecutionCounts {
    pub planned: usize,
    pub selected: usize,
    pub completed: usize,
    pub passed: usize,
    pub failed: usize,
    pub blocked: usize,
    pub skipped: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceEnvelope {
    pub schema_version: u32,
    pub producer: String,
    pub source: SourceIdentity,
    pub artifact_digest: String,
    pub feature_set: String,
    pub platform: String,
    pub outcome: Outcome,
    pub counts: ExecutionCounts,
    pub attempts: u32,
    pub required: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeEquivalentProposal {
    pub url: String,
    pub old_commit: String,
    pub new_commit: String,
    pub tree: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Blocker {
    pub id: &'static str,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CertificationInput<'a> {
    pub source: &'a SourceIdentity,
    pub policy_version: u32,
    pub layout: &'a PackageLayout,
    pub required_platforms: &'a [Platform],
    pub unsigned: &'a [UnsignedPackageManifest],
    pub distributions: &'a [DistributionManifest],
    pub evidence: &'a [EvidenceEnvelope],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageLayout {
    targets: HashMap<Platform, Vec<(String, PackageRole)>>,
    steam_depots: Vec<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidBinaryName;

impl std::fmt::Display for InvalidBinaryName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("expected a nonempty binary basename using letters, digits, '.', '-', or '_'")
    }
}

impl std::error::Error for InvalidBinaryName {}

impl PackageLayout {
    /// Build conventional `dist/` layouts for all three Steam targets from a
    /// bare executable name, without an OS-specific extension or depot IDs.
    pub fn for_binary(name: &str) -> Result<Self, InvalidBinaryName> {
        if name.is_empty()
            || matches!(name, "." | "..")
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(InvalidBinaryName);
        }
        let targets = [
            (
                Platform::LinuxSteam,
                "linux",
                "",
                "libsteam_api.so",
                "debug",
            ),
            (
                Platform::WindowsSteam,
                "windows",
                ".exe",
                "steam_api64.dll",
                "pdb",
            ),
            (
                Platform::MacosSteam,
                "macos",
                "",
                "libsteam_api.dylib",
                "dSYM",
            ),
        ]
        .into_iter()
        .map(|(platform, dir, binary_suffix, library, symbol_suffix)| {
            let files = vec![
                (
                    format!("dist/{dir}/{name}{binary_suffix}"),
                    PackageRole::Binary,
                ),
                (format!("dist/{dir}/{library}"), PackageRole::NativeLibrary),
                (
                    format!("dist/symbols/{dir}/{name}.{symbol_suffix}"),
                    PackageRole::Symbols,
                ),
            ];
            (platform, files)
        })
        .collect();
        Ok(Self {
            targets,
            steam_depots: Vec::new(),
        })
    }

    /// Replace the required paths for one target. Steam still requires a
    /// binary, a sibling native library, and symbols; Android still requires
    /// exactly one package of the corresponding type.
    pub fn with_target_files(
        mut self,
        platform: Platform,
        files: Vec<(String, PackageRole)>,
    ) -> Self {
        self.targets.insert(platform, files);
        self
    }

    pub fn with_steam_depots(mut self, ids: impl IntoIterator<Item = u32>) -> Self {
        self.steam_depots = ids.into_iter().collect();
        self
    }

    pub fn required_files(&self, platform: Platform) -> &[(String, PackageRole)] {
        self.targets.get(&platform).map_or(&[], Vec::as_slice)
    }

    pub fn steam_upload_instructions(&self) -> Vec<String> {
        std::iter::once("steam/app_build.vdf".to_owned())
            .chain(
                self.steam_depots
                    .iter()
                    .map(|id| format!("steam/depot_{id}.vdf")),
            )
            .collect()
    }
}

impl Platform {
    pub fn promotable(self) -> bool {
        !matches!(self, Self::AndroidApk)
    }
}

pub fn parent_release_ref_ok(kind: CanonicalRef) -> bool {
    matches!(kind, CanonicalRef::ReleaseTag)
}

pub fn dependency_ref_ok(kind: CanonicalRef) -> bool {
    matches!(kind, CanonicalRef::ReleaseTag | CanonicalRef::ApprovedHead)
}

pub fn tree_equivalent_authorizes_reuse(_: &TreeEquivalentProposal) -> bool {
    false
}

pub fn native_library_is_sibling(binary: &str, lib: &str) -> bool {
    match (Path::new(binary).parent(), Path::new(lib).parent()) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

pub fn missing_required(
    entries: &[PackageEntry],
    required: &[(impl AsRef<str>, PackageRole)],
) -> Vec<String> {
    required
        .iter()
        .filter(|(path, role)| {
            !entries
                .iter()
                .any(|entry| entry.path == path.as_ref() && entry.role == *role)
        })
        .map(|(path, _)| path.as_ref().to_owned())
        .collect()
}

pub fn counts_complete(counts: &ExecutionCounts) -> bool {
    counts.selected > 0
        && counts.completed == counts.selected
        && counts.passed + counts.failed + counts.blocked + counts.skipped == counts.completed
}

pub fn evidence_fresh(
    evidence: &EvidenceEnvelope,
    source: &SourceIdentity,
    artifact_digest: &str,
    feature_set: &str,
    platform: &str,
) -> bool {
    evidence.schema_version == SCHEMA_VERSION
        && evidence.source == *source
        && !source.dirty
        && evidence.artifact_digest == artifact_digest
        && evidence.feature_set == feature_set
        && evidence.platform == platform
        && evidence.attempts == 0
        && counts_complete(&evidence.counts)
}

pub fn gate_blocks(outcome: Outcome, fresh: bool, attempts: u32, required: bool) -> bool {
    attempts > 0
        || (required && !fresh)
        || matches!(outcome, Outcome::Fail | Outcome::Blocked)
        || (required && matches!(outcome, Outcome::Skipped))
}

pub fn unsigned_reproducible(
    left: &UnsignedPackageManifest,
    right: &UnsignedPackageManifest,
) -> bool {
    left == right
}

pub fn promotion_allowed(built: &DistributionManifest, promoted: &DistributionManifest) -> bool {
    built.signed
        && promoted.signed
        && built.platform == promoted.platform
        && built.unsigned_digest == promoted.unsigned_digest
        && built.package_digest == promoted.package_digest
        && built.platform.promotable()
}

pub fn certify(input: &CertificationInput<'_>) -> Vec<Blocker> {
    let mut blockers = Vec::new();
    if input.policy_version != SCHEMA_VERSION {
        blockers.push(blocker(
            "policy-version",
            format!("{} != {SCHEMA_VERSION}", input.policy_version),
        ));
    }
    if input.source.dirty {
        blockers.push(blocker(
            "dirty-workspace",
            "release certification requires a clean workspace",
        ));
    }
    if input.source.commit.is_empty() || input.source.workspace_digest.is_empty() {
        blockers.push(blocker(
            "source-identity",
            "commit and workspace digest must be nonempty",
        ));
    }
    for platform in input.required_platforms {
        let Some(unsigned) = input
            .unsigned
            .iter()
            .find(|package| package.platform == *platform)
        else {
            blockers.push(blocker(
                "missing-unsigned",
                format!("{platform:?} unsigned package is missing"),
            ));
            continue;
        };
        if unsigned.source != *input.source {
            blockers.push(blocker(
                "unsigned-source",
                format!("{platform:?} unsigned package source does not match candidate"),
            ));
        }
        let required = input.layout.required_files(*platform);
        let missing = missing_required(&unsigned.entries, required);
        if !missing.is_empty() {
            blockers.push(blocker(
                "package-composition",
                format!("{platform:?} missing {}", missing.join(", ")),
            ));
        }
        if matches!(platform, Platform::AndroidApk | Platform::AndroidAab) {
            let suffix = if *platform == Platform::AndroidApk {
                ".apk"
            } else {
                ".aab"
            };
            if !matches!(unsigned.entries.as_slice(), [entry]
                if entry.kind == EntryKind::File
                    && entry.role == PackageRole::Package
                    && entry.path.ends_with(suffix))
            {
                blockers.push(blocker(
                    "package-composition",
                    format!("{platform:?} requires exactly one {suffix} package entry"),
                ));
            }
        } else {
            if ![
                PackageRole::Binary,
                PackageRole::NativeLibrary,
                PackageRole::Symbols,
            ]
            .into_iter()
            .all(|needed| required.iter().any(|(_, role)| *role == needed))
            {
                blockers.push(blocker(
                    "package-layout",
                    format!(
                        "{platform:?} layout must require a binary, native library, and symbols"
                    ),
                ));
            }
            if !required.iter().any(|(binary, role)| {
                *role == PackageRole::Binary
                    && !missing.contains(binary)
                    && required.iter().any(|(lib, role)| {
                        *role == PackageRole::NativeLibrary
                            && !missing.contains(lib)
                            && native_library_is_sibling(binary, lib)
                    })
            }) {
                blockers.push(blocker(
                    "native-library",
                    format!("{platform:?} native library must sit beside the binary"),
                ));
            }
        }
        let Some(distribution) = input
            .distributions
            .iter()
            .find(|package| package.platform == *platform)
        else {
            blockers.push(blocker(
                "missing-distribution",
                format!("{platform:?} distribution manifest is missing"),
            ));
            continue;
        };
        if distribution.unsigned_digest != unsigned.digest {
            blockers.push(blocker(
                "unsigned-digest",
                format!("{platform:?} distribution does not bind the unsigned digest"),
            ));
        }
        if platform.promotable() && !distribution.signed {
            blockers.push(blocker(
                "unsigned-distribution",
                format!("{platform:?} promotion requires a signed distribution"),
            ));
        }
    }
    for evidence in input.evidence {
        let bound = input
            .unsigned
            .iter()
            .any(|package| package.digest == evidence.artifact_digest)
            || input
                .distributions
                .iter()
                .any(|package| package.package_digest == evidence.artifact_digest);
        let fresh = bound
            && evidence_fresh(
                evidence,
                input.source,
                &evidence.artifact_digest,
                &evidence.feature_set,
                &evidence.platform,
            );
        if gate_blocks(
            evidence.outcome,
            fresh,
            evidence.attempts,
            evidence.required,
        ) {
            blockers.push(blocker(
                "evidence",
                format!(
                    "{} result={:?} fresh={fresh} attempts={}",
                    evidence.producer, evidence.outcome, evidence.attempts
                ),
            ));
        }
    }
    blockers
}

fn blocker(id: &'static str, reason: impl Into<String>) -> Blocker {
    Blocker {
        id,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> SourceIdentity {
        SourceIdentity {
            commit: "c".into(),
            workspace_digest: "w".into(),
            dirty: false,
        }
    }

    fn env() -> BuildEnvironmentIdentity {
        BuildEnvironmentIdentity {
            target: "x86_64-unknown-linux-gnu".into(),
            profile: "release".into(),
            features: "desktop".into(),
            rustc: "1.95.0".into(),
            cargo: "1.95.0".into(),
            flake_lock_digest: "flake".into(),
            toolchain_digest: "toolchain".into(),
            sdk: "sniper".into(),
            hermetic: true,
        }
    }

    fn entry(path: &str, role: PackageRole) -> PackageEntry {
        PackageEntry {
            path: path.into(),
            digest: path.into(),
            size: 1,
            kind: if role == PackageRole::Symbols && path.ends_with("dSYM") {
                EntryKind::Dir
            } else {
                EntryKind::File
            },
            executable: role == PackageRole::Binary,
            symlink_target: None,
            arch: Some("x86_64".into()),
            role,
        }
    }

    fn linux_unsigned(layout: &PackageLayout) -> UnsignedPackageManifest {
        UnsignedPackageManifest {
            source: source(),
            environment: env(),
            platform: Platform::LinuxSteam,
            feature_set: "desktop".into(),
            cargo_lock_digest: "lock".into(),
            entries: layout
                .required_files(Platform::LinuxSteam)
                .iter()
                .map(|(path, role)| entry(path, *role))
                .collect(),
            digest: "unsigned".into(),
        }
    }

    fn counts() -> ExecutionCounts {
        ExecutionCounts {
            planned: 1,
            selected: 1,
            completed: 1,
            passed: 1,
            failed: 0,
            blocked: 0,
            skipped: 0,
        }
    }

    fn evidence(outcome: Outcome) -> EvidenceEnvelope {
        EvidenceEnvelope {
            schema_version: SCHEMA_VERSION,
            producer: "linux".into(),
            source: source(),
            artifact_digest: "unsigned".into(),
            feature_set: "desktop".into(),
            platform: "linux-steam".into(),
            outcome,
            counts: counts(),
            attempts: 0,
            required: true,
        }
    }

    #[test]
    fn fail_closed_identity_and_reuse() {
        let layout = PackageLayout::for_binary("demo").unwrap();
        let unsigned = linux_unsigned(&layout);
        let signed = DistributionManifest {
            platform: Platform::LinuxSteam,
            unsigned_digest: unsigned.digest.clone(),
            package_digest: "signed".into(),
            signed: true,
        };
        let input = CertificationInput {
            source: &source(),
            policy_version: SCHEMA_VERSION,
            layout: &layout,
            required_platforms: &[Platform::LinuxSteam],
            unsigned: std::slice::from_ref(&unsigned),
            distributions: std::slice::from_ref(&signed),
            evidence: &[],
        };
        assert_eq!(certify(&input), Vec::new());
        assert!(unsigned_reproducible(&unsigned, &linux_unsigned(&layout)));
        assert!(promotion_allowed(&signed, &signed));
        assert!(!Platform::AndroidApk.promotable());
        let apk = DistributionManifest {
            platform: Platform::AndroidApk,
            unsigned_digest: "apk".into(),
            package_digest: "apk".into(),
            signed: false,
        };
        assert!(!promotion_allowed(&apk, &apk));

        let mut android_unsigned = linux_unsigned(&layout);
        android_unsigned.platform = Platform::AndroidApk;
        android_unsigned.entries = vec![entry("app.apk", PackageRole::Binary)];
        let android_input = CertificationInput {
            source: &android_unsigned.source,
            policy_version: SCHEMA_VERSION,
            layout: &layout,
            required_platforms: &[Platform::AndroidApk],
            unsigned: std::slice::from_ref(&android_unsigned),
            distributions: std::slice::from_ref(&apk),
            evidence: &[],
        };
        assert!(
            certify(&android_input)
                .iter()
                .any(|blocker| blocker.id == "package-composition")
        );
        android_unsigned.entries[0].role = PackageRole::Package;
        let android_input = CertificationInput {
            source: &android_unsigned.source,
            policy_version: SCHEMA_VERSION,
            layout: &layout,
            required_platforms: &[Platform::AndroidApk],
            unsigned: std::slice::from_ref(&android_unsigned),
            distributions: std::slice::from_ref(&apk),
            evidence: &[],
        };
        assert!(
            certify(&android_input)
                .iter()
                .all(|blocker| blocker.id != "package-composition")
        );

        assert!(parent_release_ref_ok(CanonicalRef::ReleaseTag));
        assert!(!parent_release_ref_ok(CanonicalRef::ApprovedHead));
        assert!(dependency_ref_ok(CanonicalRef::ApprovedHead));
        assert!(!tree_equivalent_authorizes_reuse(&TreeEquivalentProposal {
            url: "git".into(),
            old_commit: "a".into(),
            new_commit: "b".into(),
            tree: "t".into(),
        }));
        assert!(!gate_blocks(Outcome::Pass, true, 0, true));
        assert!(gate_blocks(Outcome::Pass, false, 0, true));
        assert!(gate_blocks(Outcome::Skipped, true, 0, true));
        assert!(!gate_blocks(Outcome::Skipped, true, 0, false));
        assert!(gate_blocks(Outcome::Pass, true, 1, true));

        let mut dirty = source();
        dirty.dirty = true;
        let dirty_input = CertificationInput {
            source: &dirty,
            ..input
        };
        assert!(
            certify(&dirty_input)
                .iter()
                .any(|blocker| blocker.id == "dirty-workspace")
        );

        let empty_layout = layout
            .clone()
            .with_target_files(Platform::LinuxSteam, vec![]);
        assert!(
            certify(&CertificationInput {
                layout: &empty_layout,
                ..input
            })
            .iter()
            .any(|blocker| blocker.id == "package-layout")
        );

        let mut nested = linux_unsigned(&layout);
        nested.entries[1].path = "dist/linux/lib/libsteam_api.so".into();
        let nested_input = CertificationInput {
            unsigned: std::slice::from_ref(&nested),
            ..input
        };
        assert!(
            certify(&nested_input)
                .iter()
                .any(|blocker| blocker.id == "native-library")
        );

        let rebuilt = DistributionManifest {
            package_digest: "other".into(),
            ..signed.clone()
        };
        assert!(!promotion_allowed(&signed, &rebuilt));

        let mut stale = evidence(Outcome::Pass);
        stale.attempts = 2;
        let stale_input = CertificationInput {
            evidence: std::slice::from_ref(&stale),
            ..input
        };
        assert!(
            certify(&stale_input)
                .iter()
                .any(|blocker| blocker.id == "evidence")
        );

        let mut unbound = evidence(Outcome::Pass);
        unbound.artifact_digest = "unknown".into();
        let unbound_input = CertificationInput {
            evidence: std::slice::from_ref(&unbound),
            ..input
        };
        assert!(
            certify(&unbound_input)
                .iter()
                .any(|blocker| blocker.id == "evidence")
        );
    }
}
