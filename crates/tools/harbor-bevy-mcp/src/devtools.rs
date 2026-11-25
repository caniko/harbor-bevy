use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use regex::Regex;
use rmcp::{
    ErrorData,
    handler::server::wrapper::{Json, Parameters},
    tool, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::tool_result;

#[cfg(unix)]
use rustix::fs::{Mode, OFlags, open, openat};

const MAX_FILES: usize = 8;
const MAX_LINES: u64 = 500;
const MAX_SCAN_BYTES: u64 = 4 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 32 * 1024;

/// Exposes a configured directory of non-sensitive test artifacts, not the project root.
pub struct DevTools {
    #[cfg(unix)]
    artifact_root: File,
    #[cfg(not(unix))]
    artifact_root: PathBuf,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FileRange {
    pub path: PathBuf,
    /// First line to scan (1-based, inclusive).
    pub start: u64,
    /// Last line to scan (inclusive).
    pub end: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Line {
    pub number: u64,
    pub text: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FileRead {
    pub path: PathBuf,
    pub lines: Vec<Line>,
    pub truncated: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Report {
    pub sha256: String,
    pub hash_matches: Option<bool>,
    /// Parsed JSON if the report fits the response budget and is valid JSON.
    pub report: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReadInput {
    files: Vec<FileRange>,
    pattern: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReportInput {
    path: PathBuf,
    expected_sha256: Option<String>,
}

impl DevTools {
    pub fn new(artifact_root: &Path) -> Result<Self> {
        let artifact_root = artifact_root
            .canonicalize()
            .context("resolve artifact directory")?;
        ensure!(artifact_root.is_dir(), "artifact root must be a directory");
        #[cfg(unix)]
        let artifact_root =
            File::from(open(artifact_root, Self::directory_flags(), Mode::empty())?);
        Ok(Self { artifact_root })
    }

    fn open(&self, path: &Path) -> Result<File> {
        ensure!(
            path.components().all(|c| matches!(c, Component::Normal(_)))
                && !path.as_os_str().is_empty(),
            "expected a relative artifact path"
        );
        let file = self.open_relative(path)?;
        ensure!(
            file.metadata()?.is_file(),
            "expected a regular artifact file"
        );
        Ok(file)
    }

    #[cfg(unix)]
    fn directory_flags() -> OFlags {
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
    }

    #[cfg(unix)]
    fn open_relative(&self, path: &Path) -> Result<File> {
        let mut components = path.components();
        let file = components.next_back().context("missing artifact path")?;
        let mut directory = None;
        for component in components {
            directory = Some(File::from(
                openat(
                    directory.as_ref().unwrap_or(&self.artifact_root),
                    component.as_os_str(),
                    Self::directory_flags(),
                    Mode::empty(),
                )
                .context("open artifact directory")?,
            ));
        }
        Ok(File::from(
            openat(
                directory.as_ref().unwrap_or(&self.artifact_root),
                file.as_os_str(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .context("open artifact")?,
        ))
    }

    #[cfg(not(unix))]
    fn open_relative(&self, path: &Path) -> Result<File> {
        // ponytail: the portable fallback assumes only trusted processes can
        // rename entries in the artifact directory. Unix uses anchored opens.
        let path = self
            .artifact_root
            .join(path)
            .canonicalize()
            .context("resolve artifact path")?;
        ensure!(
            path.starts_with(&self.artifact_root) && path.is_file(),
            "expected a file within artifact root"
        );
        File::open(path).context("open artifact")
    }

    /// Read bounded ranges of log or source text, optionally filtering by a Rust regex.
    pub fn read_ranges(&self, files: &[FileRange], pattern: Option<&str>) -> Result<Vec<FileRead>> {
        ensure!(
            !files.is_empty() && files.len() <= MAX_FILES,
            "provide 1 to {MAX_FILES} ranges"
        );
        ensure!(
            pattern.is_none_or(|p| p.len() <= 1024),
            "pattern is too long"
        );
        let pattern = pattern.map(Regex::new).transpose()?;
        let mut budget = MAX_OUTPUT_BYTES;
        files
            .iter()
            .map(|range| {
                ensure!(
                    range.start > 0
                        && range.end >= range.start
                        && range.end - range.start < MAX_LINES,
                    "provide an inclusive 1-based range of at most {MAX_LINES} lines"
                );
                let mut file = BufReader::new(self.open(&range.path)?.take(MAX_SCAN_BYTES));
                let mut lines = Vec::new();
                let mut buf = Vec::new();
                let mut truncated = false;
                for number in 1..=range.end {
                    buf.clear();
                    if file.read_until(b'\n', &mut buf)? == 0 {
                        truncated = file.get_ref().limit() == 0;
                        break;
                    }
                    if file.get_ref().limit() == 0 && !buf.ends_with(b"\n") {
                        truncated = true;
                        break;
                    }
                    if number < range.start {
                        continue;
                    }
                    let text = String::from_utf8_lossy(&buf);
                    let text = text.trim_end_matches(['\r', '\n']);
                    if pattern.as_ref().is_some_and(|p| !p.is_match(text)) {
                        continue;
                    }
                    let cost = text.len() + 32; // Account for line metadata, including empty lines.
                    if cost > budget {
                        truncated = true;
                        break;
                    }
                    budget -= cost;
                    lines.push(Line {
                        number,
                        text: text.to_owned(),
                    });
                }
                Ok(FileRead {
                    path: range.path.clone(),
                    lines,
                    truncated,
                })
            })
            .collect()
    }

    /// Inspect JSON and compare the hash of the exact bytes, without changing the report.
    pub fn inspect_report(&self, path: &Path, expected_sha256: Option<&str>) -> Result<Report> {
        ensure!(
            expected_sha256
                .is_none_or(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())),
            "expected_sha256 must contain 64 hexadecimal digits"
        );
        let mut data = Vec::new();
        self.open(path)?
            .take(MAX_SCAN_BYTES + 1)
            .read_to_end(&mut data)?;
        ensure!(
            data.len() as u64 <= MAX_SCAN_BYTES,
            "report exceeds inspection limit"
        );
        let sha256: String = Sha256::digest(&data)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let hash_matches = expected_sha256.map(|expected| sha256.eq_ignore_ascii_case(expected));
        let report = if data.len() <= MAX_OUTPUT_BYTES {
            serde_json::from_slice(&data).ok()
        } else {
            None
        };
        Ok(Report {
            sha256,
            hash_matches,
            report,
        })
    }
}

pub(crate) fn tool_set(state: DevTools) -> impl mcp_layer::ToolSet {
    mcp_layer::Routed {
        state,
        router: DevTools::tool_router(),
    }
}

#[tool_router]
impl DevTools {
    #[tool(
        name = "read_ranges",
        description = "Read bounded ranges from authorized log files and optionally filter by regex."
    )]
    fn read_ranges_tool(
        &self,
        Parameters(input): Parameters<ReadInput>,
    ) -> Result<Json<Vec<FileRead>>, ErrorData> {
        tool_result(self.read_ranges(&input.files, input.pattern.as_deref()))
    }

    #[tool(
        name = "inspect_report",
        description = "Inspect a JSON report and optionally verify its SHA-256."
    )]
    fn report_tool(
        &self,
        Parameters(input): Parameters<ReportInput>,
    ) -> Result<Json<Report>, ErrorData> {
        tool_result(self.inspect_report(&input.path, input.expected_sha256.as_deref()))
    }
}
