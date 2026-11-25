//! End-to-end wire smoke test: spins up two clients against a running
//! `thespan-rendezvous` server, announces from one, discovers from the
//! other, and prints the discovered ticket.
//!
//! Usage:
//!   # terminal 1:
//!   cargo run -p thespan-rendezvous-server -- \
//!     --listen-quic /ip4/127.0.0.1/udp/4001/quic-v1 \
//!     --listen-tcp  /ip4/127.0.0.1/tcp/4001 \
//!     --key-path    /tmp/rdv-server.key
//!
//!   # terminal 2:
//!   cargo run -p thespan-rendezvous-client --example smoke -- \
//!     --bootstrap /ip4/127.0.0.1/udp/4001/quic-v1
//!
//! The server's stdout prints its peer id at startup; the smoke binary takes
//! the listen multiaddr, with or without the trailing `/p2p/<peer>`, and dials it.

use anyhow::{Context, Result};
use libp2p::{Multiaddr, identity::Keypair};
use std::time::Duration;
use thespan_rendezvous_client::{Client, ClientConfig};
use thespan_rendezvous_proto::{MultiaddrBytes, Namespace, PeerIdBytes, PeerTicket};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let server_addr: Multiaddr = parse_bootstrap_arg()?
        .parse()
        .context("parse server multiaddr")?;

    // Two independent clients, two independent keypairs.
    let announcer_key = Keypair::generate_ed25519();
    let discoverer_key = Keypair::generate_ed25519();

    let announcer = Client::start(ClientConfig::new(
        announcer_key.clone(),
        vec![server_addr.clone()],
    ))
    .await?;
    let discoverer =
        Client::start(ClientConfig::new(discoverer_key, vec![server_addr.clone()])).await?;

    // Give the swarms a moment to dial + handshake.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let ns = Namespace::new("lobby.public").unwrap();
    let ticket = PeerTicket {
        peer_id: PeerIdBytes(announcer_key.public().to_peer_id().to_bytes()),
        addrs: vec![MultiaddrBytes("/ip4/127.0.0.1/udp/9999/quic-v1".into())],
        relay_hints: vec![],
    };

    let _announcement = announcer
        .announce(
            ns.clone(),
            ticket.clone(),
            b"smoke=test".to_vec(),
            Duration::from_secs(60),
        )
        .await
        .context("announce")?;
    println!("[smoke] announced under {}", ns.as_str());

    let entries = discoverer.discover(ns.clone()).await.context("discover")?;
    println!("[smoke] discovered {} entries:", entries.len());
    for e in &entries {
        println!(
            "  peer={} addrs={:?} ttl_remaining={}s metadata={:?}",
            hex(&e.ticket.peer_id.0),
            e.ticket.addrs.iter().map(|a| &a.0).collect::<Vec<_>>(),
            e.ttl_remaining_secs,
            std::str::from_utf8(&e.metadata).unwrap_or("<bin>"),
        );
    }

    if entries.is_empty() {
        anyhow::bail!("smoke test failed: empty discover");
    }
    println!("[smoke] OK");
    Ok(())
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b {
        use std::fmt::Write;
        let _ = write!(s, "{byte:02x}");
    }
    s
}

fn parse_bootstrap_arg() -> Result<String> {
    let mut args = std::env::args().skip(1);
    let Some(first) = args.next() else {
        anyhow::bail!("usage: smoke [--bootstrap] <server_multiaddr>");
    };
    if first == "--bootstrap" {
        return args
            .next()
            .context("usage: smoke --bootstrap <server_multiaddr>");
    }
    Ok(first)
}
