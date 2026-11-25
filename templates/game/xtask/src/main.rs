use anyhow::{Result, bail};
use clap::Parser;
use harbor_bevy_xtask::{KitCommand, Project, ProjectCommand, ProjectHooks};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: KitCommand,
}

struct Hooks;

impl ProjectHooks for Hooks {
    fn run(&self, _: &Project<'_>, command: ProjectCommand) -> Result<()> {
        bail!("project hook not implemented: {command:?}")
    }
}

fn main() -> Result<()> {
    Cli::parse().command.run(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .as_path(),
        &Hooks,
    )
}
