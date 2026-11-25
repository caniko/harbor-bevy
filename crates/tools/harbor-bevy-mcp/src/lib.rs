//! Composable development and playtest tools.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use mcp_layer::{Layered, Routed};
use rmcp::{
    ErrorData,
    handler::server::wrapper::{Json, Parameters},
    model::{ServerCapabilities, ServerConfig},
    tool, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

mod devtools;
pub use devtools::{DevTools, FileRange, FileRead, Line, Report};

/// The command prefix and working directory are supplied by the embedding application.
pub struct XtaskRunner {
    root: PathBuf,
    program: PathBuf,
}

impl XtaskRunner {
    pub fn new(root: &Path, program: &Path) -> Result<Self> {
        let root = root.canonicalize()?;
        ensure!(root.is_dir(), "xtask root must be a directory");
        // A bare name is resolved on PATH. Relative paths with directories
        // are resolved against the configured root, not the server's CWD.
        let program = if program.components().count() > 1 && program.is_relative() {
            root.join(program)
        } else {
            program.into()
        };
        ensure!(!program.as_os_str().is_empty(), "xtask program is empty");
        Ok(Self { root, program })
    }

    pub fn run(&self, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Result<Output> {
        Command::new(&self.program)
            .args(args)
            .current_dir(&self.root)
            .output()
            .with_context(|| format!("execute {}", self.program.display()))
    }
}

/// Project-owned session behavior. Implementations own validation of session IDs,
/// launch options and game actions, and must serialize concurrent calls as needed.
pub trait Playtest: Send + Sync + 'static {
    fn start_session(&self, options: Value) -> Result<Value>;
    fn get_state(&self, session_id: &str) -> Result<Value>;
    fn execute_action(&self, session_id: &str, action: Value) -> Result<Value>;
    fn stop_session(&self, session_id: &str) -> Result<Value>;
}

#[derive(Debug, Deserialize, JsonSchema)]
struct StartInput {
    #[serde(default)]
    options: Value,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SessionInput {
    session_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ActionInput {
    session_id: String,
    action: Value,
}

struct PlaytestTools(Arc<dyn Playtest>);

fn tool_result<T>(result: Result<T>) -> std::result::Result<Json<T>, ErrorData> {
    result
        .map(Json)
        .map_err(|error| ErrorData::invalid_params(error.to_string(), None))
}

#[tool_router]
impl PlaytestTools {
    #[tool(description = "Start a project-defined playtest session.")]
    fn start_session(
        &self,
        Parameters(input): Parameters<StartInput>,
    ) -> Result<Json<Value>, ErrorData> {
        tool_result(self.0.start_session(input.options))
    }

    #[tool(description = "Read a playtest session's current state.")]
    fn get_state(
        &self,
        Parameters(input): Parameters<SessionInput>,
    ) -> Result<Json<Value>, ErrorData> {
        tool_result(self.0.get_state(&input.session_id))
    }

    #[tool(description = "Execute a project-defined action in a playtest session.")]
    fn execute_action(
        &self,
        Parameters(input): Parameters<ActionInput>,
    ) -> Result<Json<Value>, ErrorData> {
        tool_result(self.0.execute_action(&input.session_id, input.action))
    }

    #[tool(description = "Stop a playtest session and release its resources.")]
    fn stop_session(
        &self,
        Parameters(input): Parameters<SessionInput>,
    ) -> Result<Json<Value>, ErrorData> {
        tool_result(self.0.stop_session(&input.session_id))
    }
}

fn with_project_tools(
    server: Layered,
    devtools: DevTools,
    playtest: Option<Arc<dyn Playtest>>,
) -> Layered {
    let server = server.with(devtools::tool_set(devtools));
    if let Some(playtest) = playtest {
        server.with(Routed {
            state: PlaytestTools(playtest),
            router: PlaytestTools::tool_router(),
        })
    } else {
        server
    }
}

/// Default server: only read-only artifact inspection and the supplied playtest backend.
pub fn layered(devtools: DevTools, playtest: Option<Arc<dyn Playtest>>) -> Layered {
    let info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build());
    with_project_tools(Layered::new(info), devtools, playtest)
}

/// The pinned Fragpipe MCP feature itself depends on Steampipe, so this adapter
/// is opt-in along with the cluster tools. The default server uses only mcp-layer.
#[cfg(feature = "cluster")]
pub fn layered_with_cluster(
    fragpipe: fragpipe::mcp::FragpipeCtx,
    steampipe: steampipe::mcp::SteampipeCtx,
    jobs: mcp_layer::job_tools::JobTools,
    devtools: DevTools,
    playtest: Option<Arc<dyn Playtest>>,
) -> Layered {
    with_project_tools(
        fragpipe::mcp::layered(fragpipe, steampipe, jobs),
        devtools,
        playtest,
    )
}

#[cfg(test)]
mod tests;
