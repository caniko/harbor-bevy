//! Worktree policy for tracked and unignored UTF-8 text files.

use std::{
    ffi::OsStr,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};
use regex::RegexSetBuilder;

#[derive(Debug, PartialEq, Eq)]
pub struct Hit {
    pub path: PathBuf,
    pub line: usize,
}

pub fn scan(root: &Path, patterns: &[String]) -> Result<Vec<Hit>> {
    if patterns.is_empty() {
        return Ok(Vec::new());
    }
    let patterns = RegexSetBuilder::new(patterns)
        .case_insensitive(true)
        .build()?;
    let root = root.canonicalize()?;
    let output = Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--deduplicate",
        ])
        .current_dir(&root)
        .output()
        .context("list worktree files with Git")?;
    ensure!(
        output.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut hits = Vec::new();
    for raw in output
        .stdout
        .split(|&byte| byte == 0)
        .filter(|p| !p.is_empty())
    {
        #[cfg(unix)]
        let path = {
            use std::os::unix::ffi::OsStrExt;
            Path::new(OsStr::from_bytes(raw))
        };
        #[cfg(not(unix))]
        let path = Path::new(OsStr::new(std::str::from_utf8(raw)?));
        let full = root.join(path);
        let metadata = match fs::symlink_metadata(&full) {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("inspect {}", path.display())),
        };
        if !metadata.is_file() || !full.canonicalize()?.starts_with(&root) {
            continue;
        }
        let data = fs::read(&full).with_context(|| format!("read {}", path.display()))?;
        if !data.contains(&0)
            && let Ok(text) = std::str::from_utf8(&data)
        {
            hits.extend(
                text.lines()
                    .enumerate()
                    .filter(|(_, line)| patterns.is_match(line))
                    .map(|(offset, _)| Hit {
                        path: path.into(),
                        line: offset + 1,
                    }),
            );
        }
    }
    hits.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    hits.dedup();
    Ok(hits)
}
