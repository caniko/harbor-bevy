//! Persistent server identity (ed25519 keypair).

use anyhow::{Context, Result};
use libp2p::identity::Keypair;
use std::path::Path;
use tracing::info;

pub fn load_or_create(path: &Path) -> Result<Keypair> {
    if path.exists() {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let kp = Keypair::from_protobuf_encoding(&bytes)
            .with_context(|| format!("decode keypair from {}", path.display()))?;
        info!(peer_id = %kp.public().to_peer_id(), "loaded persistent identity");
        return Ok(kp);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let kp = Keypair::generate_ed25519();
    let bytes = kp.to_protobuf_encoding().context("encode keypair")?;
    std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))?;
    info!(peer_id = %kp.public().to_peer_id(), "generated new identity");
    Ok(kp)
}
