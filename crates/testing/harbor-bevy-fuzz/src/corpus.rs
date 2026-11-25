//! Content-addressed JSON traces. The corpus ID hashes the exact on-disk bytes.

use std::{
    fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use harbor_bevy_headless::{FaultInjectionEnvelope, HeadlessConfig};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// The replay-relevant settings for a single app. The seed is stored separately
/// in [`CorpusEntry`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceConfig {
    pub fixed_dt: Duration,
    /// Total update limit including the startup frame.
    pub frame_budget: Option<u64>,
    pub enable_logging: bool,
    /// Frames to run after each action unless the app reaches an outcome first.
    pub frames_per_action: u64,
}

impl Default for TraceConfig {
    fn default() -> Self {
        let headless = HeadlessConfig::default();
        Self {
            fixed_dt: headless.fixed_dt,
            frame_budget: headless.frame_budget,
            enable_logging: headless.enable_logging,
            frames_per_action: 1,
        }
    }
}

/// A concrete trace, independent of the generator and proptest's RNG.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusEntry<A> {
    pub name: String,
    pub seed: u64,
    pub config: TraceConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fault_envelope: Option<FaultInjectionEnvelope>,
    pub actions: Vec<A>,
}

/// A directory of `<16 lowercase hex characters>.json` corpus files.
#[derive(Clone, Debug)]
pub struct CorpusStore {
    dir: PathBuf,
}

impl CorpusStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Atomically publish an entry, returning the existing path on an identical
    /// save. Never overwrite different contents at the same truncated hash.
    pub fn save<A: Serialize>(&self, entry: &CorpusEntry<A>) -> anyhow::Result<PathBuf> {
        let mut bytes = serde_json::to_vec_pretty(entry).context("serialize corpus entry")?;
        bytes.push(b'\n');
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("create corpus directory {}", self.dir.display()))?;
        let path = self.dir.join(format!("{}.json", content_id(&bytes)));
        let mut temp = tempfile::NamedTempFile::new_in(&self.dir)
            .with_context(|| format!("create temporary file in {}", self.dir.display()))?;
        temp.write_all(&bytes)?;
        temp.as_file().sync_all()?;
        match temp.persist_noclobber(&path) {
            Ok(_) => {}
            Err(error) if error.error.kind() == ErrorKind::AlreadyExists => {
                anyhow::ensure!(
                    read(&path)? == bytes,
                    "different contents already stored at {}",
                    path.display()
                );
            }
            Err(error) => return Err(error).with_context(|| format!("publish {}", path.display())),
        }
        Ok(path)
    }

    /// Read an entry by ID, checking that the contents still match that ID.
    pub fn load<A: DeserializeOwned>(&self, id: &str) -> anyhow::Result<CorpusEntry<A>> {
        anyhow::ensure!(valid_id(id), "invalid corpus ID {id:?}");
        let path = self.dir.join(format!("{id}.json"));
        let bytes = read(&path)?;
        anyhow::ensure!(
            content_id(&bytes) == id,
            "hash mismatch for {}",
            path.display()
        );
        decode(&path, &bytes)
    }

    /// Read a JSON trace at an explicit path, without requiring a hash filename.
    pub fn load_file<A: DeserializeOwned>(
        path: impl AsRef<Path>,
    ) -> anyhow::Result<CorpusEntry<A>> {
        let path = path.as_ref();
        decode(path, &read(path)?)
    }

    /// List sorted IDs, ignoring unrelated files. A missing directory is empty.
    pub fn list(&self) -> anyhow::Result<Vec<String>> {
        let items = match fs::read_dir(&self.dir) {
            Ok(items) => items,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| format!("list corpus {}", self.dir.display()));
            }
        };
        let mut ids = Vec::new();
        for item in items {
            let item = item?;
            if item.file_type()?.is_file()
                && let Some(id) = item
                    .file_name()
                    .to_str()
                    .and_then(|name| name.strip_suffix(".json"))
                && valid_id(id)
            {
                ids.push(id.to_owned());
            }
        }
        ids.sort_unstable();
        Ok(ids)
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 16
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn content_id(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex()[..16].to_owned()
}

fn read(path: &Path) -> anyhow::Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("read {}", path.display()))
}

fn decode<A: DeserializeOwned>(path: &Path, bytes: &[u8]) -> anyhow::Result<CorpusEntry<A>> {
    serde_json::from_slice(bytes).with_context(|| format!("parse {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_ids_and_corrupted_content_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let store = CorpusStore::new(dir.path());
        let entry = CorpusEntry {
            name: "test".into(),
            seed: 9,
            config: TraceConfig::default(),
            fault_envelope: None,
            actions: vec![1u8],
        };
        let path = store.save(&entry).unwrap();
        assert_eq!(store.save(&entry).unwrap(), path);
        assert!(store.load::<u8>("../../other").is_err());

        fs::write(&path, b"corrupt").unwrap();
        let id = path.file_stem().unwrap().to_str().unwrap();
        assert!(
            store
                .load::<u8>(id)
                .unwrap_err()
                .to_string()
                .contains("hash mismatch")
        );
        assert!(store.save(&entry).is_err());
        assert_eq!(fs::read(path).unwrap(), b"corrupt");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
