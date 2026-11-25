use anyhow::Result;
use clap::Parser;
use thespan_rendezvous_server::{Config, init_tracing, run};

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = Config::parse();
    init_tracing(cfg.log_json)?;
    run(cfg).await
}
