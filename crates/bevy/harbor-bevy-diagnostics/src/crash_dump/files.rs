use serde_json::{Map, Value, json};
use std::{
    fs,
    io::{self, Write as _},
    path::Path,
    time::{Duration, SystemTime},
};

const BUNDLE_KIND: &str = "diagnostics-crash-bundle";

pub(super) fn write_manifest(
    dir: &Path,
    app_name: &str,
    session_id: &str,
    created_at: &str,
) -> io::Result<()> {
    let mut files = Map::new();
    for (key, name) in [
        ("app_log", "app.log"),
        ("state_snapshot", "state_snapshot.txt"),
        ("recent_events", "recent_events.json"),
        ("metrics", "metrics.json"),
        ("event_audit", "event_audit.json"),
        ("state_hashes", "state_hashes.json"),
        ("panic", "panic.txt"),
        ("fatal_error", "fatal_error.json"),
        ("minidumper_report", "minidumper_report.txt"),
    ] {
        if dir.join(name).is_file() {
            files.insert(key.to_string(), Value::from(name));
        }
    }
    let mut minidumps = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if entry.file_type()?.is_file()
            && let Some(name) = name.to_str()
            && name.starts_with("captured-")
            && name.ends_with(".dmp")
        {
            minidumps.push(name.to_owned());
        }
    }
    minidumps.sort_unstable();
    let manifest = json!({
        "kind": BUNDLE_KIND,
        "app_name": app_name,
        "session_id": session_id,
        "created_at": created_at,
        "files": files,
        "minidumps": minidumps,
    });
    write_file(&dir.join("manifest.json"), manifest.to_string().as_bytes())
}

pub(super) fn read_manifest(dir: &Path, session_id: &str) -> io::Result<Value> {
    let manifest: Value = serde_json::from_slice(&fs::read(dir.join("manifest.json"))?)?;
    if manifest["kind"] == BUNDLE_KIND && manifest["session_id"] == session_id {
        Ok(manifest)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a diagnostics crash bundle",
        ))
    }
}

/// Remove old bundle directories by modification time. Unrecognized folders
/// and symbolic links are ignored. Use a dedicated root outside active sessions.
pub fn cleanup_old_crash_dumps(root: &Path, retention_days: u32) -> io::Result<()> {
    let entries = match fs::read_dir(root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        result => result?,
    };
    let Some(cutoff) =
        SystemTime::now().checked_sub(Duration::from_secs(u64::from(retention_days) * 86_400))
    else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() || entry.metadata()?.modified()? >= cutoff {
            continue;
        }
        let name = entry.file_name();
        if name
            .to_str()
            .is_some_and(|id| read_manifest(&entry.path(), id).is_ok())
        {
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

/// Replace artifacts atomically so readers do not see a partial file.
pub(super) fn write_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    let temporary = path.with_file_name(format!(".write-{}", uuid::Uuid::new_v4()));
    let mut file = fs::File::create_new(&temporary)?;
    let result = file.write_all(contents);
    drop(file);
    let result = result.and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
