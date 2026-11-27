use std::{borrow::Cow, collections::BTreeMap, io, path::Path};
#[cfg(unix)]
use std::{collections::BTreeSet, ffi::CStr};

use sha2::{Digest, Sha256};
use thiserror::Error;

const MANIFEST: &str = "SHA256SUMS";

/// Trusted inputs. Both byte limits include `SHA256SUMS`.
#[derive(Clone, Debug)]
pub struct AssetPackPolicy {
    pub sha256sums_digest: [u8; 32],
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
}

/// Authenticated bytes, independent of subsequent file changes.
#[derive(Debug)]
pub struct VerifiedAssetPack<'a> {
    manifest_digest: [u8; 32],
    members: BTreeMap<String, Cow<'a, [u8]>>,
}

impl VerifiedAssetPack<'_> {
    pub fn manifest_digest(&self) -> [u8; 32] {
        self.manifest_digest
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.members.get(name).map(Cow::as_ref)
    }

    pub fn members(&self) -> impl ExactSizeIterator<Item = (&str, &[u8])> {
        self.members
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_ref()))
    }
}

#[derive(Debug, Error)]
pub enum AssetPackError {
    #[error("I/O reading {member}: {source}")]
    Io {
        member: String,
        #[source]
        source: io::Error,
    },
    #[error("not a regular file or expected directory: {0}")]
    NonRegular(String),
    #[error("byte budget exceeded at {0}")]
    ByteBudget(String),
    #[error("digest mismatch for {0}")]
    DigestMismatch(String),
    #[error("invalid SHA256SUMS at line {line}")]
    InvalidManifest { line: usize },
    #[error("unexpected or duplicate pack member: {0}")]
    UnexpectedMember(String),
    #[error("missing pack member: {0}")]
    MissingMember(String),
    #[error("race-resistant directory verification is not supported on this platform")]
    UnsupportedFilesystem,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.trim() == name
        && name != "."
        && !name.eq_ignore_ascii_case(MANIFEST)
        && !name.contains(['/', '\\', ':'])
        && !name.contains("..")
        && !name.chars().any(char::is_control)
}

fn diagnostic_name(name: &str) -> String {
    const MAX_BYTES: usize = 128;
    if name.len() <= MAX_BYTES {
        name.into()
    } else {
        format!("{}...", &name[..name.floor_char_boundary(MAX_BYTES - 3)])
    }
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn parse_manifest<'a>(
    sums: &'a [u8],
    policy: &AssetPackPolicy,
) -> Result<BTreeMap<&'a str, [u8; 32]>, AssetPackError> {
    if sums.len() > policy.max_file_bytes.min(policy.max_total_bytes) {
        return Err(AssetPackError::ByteBudget(MANIFEST.into()));
    }
    if digest(sums) != policy.sha256sums_digest {
        return Err(AssetPackError::DigestMismatch(MANIFEST.into()));
    }
    let text = std::str::from_utf8(sums)
        .ok()
        .and_then(|s| s.strip_suffix('\n'))
        .ok_or(AssetPackError::InvalidManifest { line: 1 })?;
    let mut previous = "";
    let mut entries = BTreeMap::new();
    for (i, line) in text.split('\n').enumerate() {
        let invalid = || AssetPackError::InvalidManifest { line: i + 1 };
        let (hash, name) = line.split_once("  ").ok_or_else(invalid)?;
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !valid_name(name)
            || name <= previous
        {
            return Err(invalid());
        }
        let nibble = |b: u8| {
            if b.is_ascii_digit() {
                b - b'0'
            } else {
                b - b'a' + 10
            }
        };
        let hash = hash.as_bytes();
        entries.insert(
            name,
            std::array::from_fn(|i| nibble(hash[2 * i]) << 4 | nibble(hash[2 * i + 1])),
        );
        previous = name;
    }
    Ok(entries)
}

fn verify_members<'a>(
    entries: BTreeMap<&str, [u8; 32]>,
    manifest_bytes: usize,
    policy: &AssetPackPolicy,
    mut read: impl FnMut(&str, usize) -> Result<Cow<'a, [u8]>, AssetPackError>,
) -> Result<VerifiedAssetPack<'a>, AssetPackError> {
    let mut remaining = policy.max_total_bytes - manifest_bytes;
    let mut members = BTreeMap::new();
    for (name, hash) in entries {
        let limit = remaining.min(policy.max_file_bytes);
        let bytes = read(name, limit)?;
        if bytes.len() > limit {
            return Err(AssetPackError::ByteBudget(diagnostic_name(name)));
        }
        if digest(&bytes) != hash {
            return Err(AssetPackError::DigestMismatch(diagnostic_name(name)));
        }
        remaining -= bytes.len();
        members.insert(name.into(), bytes);
    }
    Ok(VerifiedAssetPack {
        manifest_digest: policy.sha256sums_digest,
        members,
    })
}

/// Verifies the manifest and the exact embedded member set without copying asset bytes.
/// Pass `SHA256SUMS` separately, not as a member.
pub fn load_and_verify_embedded<'a>(
    sums: &[u8],
    members: &[(&str, &'a [u8])],
    policy: &AssetPackPolicy,
) -> Result<VerifiedAssetPack<'a>, AssetPackError> {
    let entries = parse_manifest(sums, policy)?;
    let mut provided = BTreeMap::new();
    for &(name, bytes) in members {
        if !entries.contains_key(name) || provided.insert(name, bytes).is_some() {
            return Err(AssetPackError::UnexpectedMember(diagnostic_name(name)));
        }
    }
    verify_members(entries, sums.len(), policy, |name, _| {
        provided
            .remove(name)
            .map(Cow::Borrowed)
            .ok_or_else(|| AssetPackError::MissingMember(diagnostic_name(name)))
    })
}

#[cfg(unix)]
fn observe_name(expected: &mut BTreeSet<&str>, name: &CStr) -> Result<(), AssetPackError> {
    if name.to_str().is_ok_and(|name| expected.remove(name)) {
        Ok(())
    } else {
        Err(AssetPackError::UnexpectedMember(diagnostic_name(
            &name.to_string_lossy(),
        )))
    }
}

/// Verifies a flat directory on Unix. Members are opened relative to the pinned
/// directory handle; returned bytes are authenticated, not a live filesystem view.
/// Exact snapshot verification assumes the directory is not mutated during
/// verification. On non-Linux Unix, the directory must be immutable during
/// verification to prevent device opens. Linux requires `/proc/self/fd`.
#[cfg(unix)]
pub fn load_and_verify_dir(
    dir: &Path,
    policy: &AssetPackPolicy,
) -> Result<VerifiedAssetPack<'static>, AssetPackError> {
    use rustix::fs::{self, AtFlags, Dir, FileType, Mode, OFlags};
    #[cfg(target_os = "linux")]
    use std::os::fd::AsRawFd;
    use std::{
        fs::{File, symlink_metadata},
        io::Read,
    };

    fn io_error(member: &str, source: impl Into<io::Error>) -> AssetPackError {
        AssetPackError::Io {
            member: diagnostic_name(member),
            source: source.into(),
        }
    }

    // Strip trailing separators before applying NOFOLLOW to the final component.
    let normalized;
    let dir = if let Some((parent, name)) = dir.parent().zip(dir.file_name()) {
        normalized = parent.join(name);
        &normalized
    } else {
        dir
    };
    let root = dir.to_string_lossy();
    if !symlink_metadata(dir)
        .map_err(|e| io_error(&root, e))?
        .is_dir()
    {
        return Err(AssetPackError::NonRegular(diagnostic_name(&root)));
    }
    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let dir = fs::open(
        dir,
        flags | OFlags::NOFOLLOW | OFlags::DIRECTORY,
        Mode::empty(),
    )
    .map_err(|e| io_error(&root, e))?;
    let load = |name: &str, limit: usize| {
        let non_regular = || AssetPackError::NonRegular(diagnostic_name(name));
        let is_regular =
            |stat: &fs::Stat| FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile;
        #[cfg(target_os = "linux")]
        let (fd, _pinned, pinned_stat) = {
            // Pin the inode without invoking a device's open handler.
            let pinned = fs::openat(
                &dir,
                name,
                OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| io_error(name, e))?;
            let stat = fs::fstat(&pinned).map_err(|e| io_error(name, e))?;
            if !is_regular(&stat) {
                return Err(non_regular());
            }
            let proc_path = format!("/proc/self/fd/{}", pinned.as_raw_fd());
            let fd = fs::open(proc_path, flags, Mode::empty()).map_err(|e| io_error(name, e))?;
            (fd, pinned, stat)
        };
        #[cfg(all(unix, not(target_os = "linux")))]
        let fd = {
            let stat =
                fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|e| io_error(name, e))?;
            if !is_regular(&stat) {
                return Err(non_regular());
            }
            fs::openat(&dir, name, flags | OFlags::NOFOLLOW, Mode::empty())
                .map_err(|e| io_error(name, e))?
        };
        let stat = fs::fstat(&fd).map_err(|e| io_error(name, e))?;
        if !is_regular(&stat) {
            return Err(non_regular());
        }
        #[cfg(target_os = "linux")]
        if (stat.st_dev, stat.st_ino) != (pinned_stat.st_dev, pinned_stat.st_ino) {
            return Err(non_regular());
        }
        let limit = limit as u64;
        if stat.st_size as u64 > limit {
            return Err(AssetPackError::ByteBudget(diagnostic_name(name)));
        }
        let mut bytes = Vec::new();
        File::from(fd)
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| io_error(name, e))?;
        if bytes.len() as u64 > limit {
            return Err(AssetPackError::ByteBudget(diagnostic_name(name)));
        }
        Ok(bytes)
    };
    let sums = load(MANIFEST, policy.max_file_bytes.min(policy.max_total_bytes))?;
    let entries = parse_manifest(&sums, policy)?;
    let pack = verify_members(entries, sums.len(), policy, |name, limit| {
        load(name, limit).map(Cow::Owned)
    })?;
    let mut expected: BTreeSet<_> = pack.members.keys().map(String::as_str).collect();
    expected.insert(MANIFEST);
    for entry in Dir::read_from(&dir).map_err(|e| io_error(&root, e))? {
        let entry = entry.map_err(|e| io_error(&root, e))?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        observe_name(&mut expected, name)?;
        if FileType::from_raw_mode(
            fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|e| io_error(&name.to_string_lossy(), e))?
                .st_mode,
        ) != FileType::RegularFile
        {
            return Err(AssetPackError::NonRegular(diagnostic_name(
                &name.to_string_lossy(),
            )));
        }
    }
    if let Some(name) = expected.first() {
        return Err(AssetPackError::MissingMember(diagnostic_name(name)));
    }
    Ok(pack)
}

#[cfg(all(test, unix))]
#[test]
fn duplicate_observed_name_cannot_replace_missing_member() {
    let mut expected = BTreeSet::from(["first.bin", "second.bin"]);
    observe_name(&mut expected, c"first.bin").unwrap();
    assert!(matches!(
        observe_name(&mut expected, c"first.bin"),
        Err(AssetPackError::UnexpectedMember(name)) if name == "first.bin"
    ));
    assert_eq!(expected, BTreeSet::from(["second.bin"]));
}

/// Fails closed on platforms without a race-resistant directory backend.
#[cfg(not(unix))]
pub fn load_and_verify_dir(
    _dir: &Path,
    _policy: &AssetPackPolicy,
) -> Result<VerifiedAssetPack<'static>, AssetPackError> {
    Err(AssetPackError::UnsupportedFilesystem)
}
