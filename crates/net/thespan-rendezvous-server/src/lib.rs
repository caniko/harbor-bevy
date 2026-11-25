pub mod behaviour;
pub mod codec;
pub mod config;
pub mod handler;
pub mod identity;
pub mod metrics;
pub mod store;
pub mod verify;

use anyhow::{Context, Result};
use behaviour::Behaviour;
pub use config::Config;
use futures::StreamExt;
use handler::Handler;
use libp2p::{Multiaddr, relay, request_response, swarm::SwarmEvent};
use metrics::Metrics;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use store::Store;
use tracing::{info, warn};

pub fn init_tracing(json: bool) -> Result<()> {
    let env_filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());

    if json {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(env_filter)
            .try_init()
            .map_err(|error| anyhow::anyhow!(error))?;
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .try_init()
            .map_err(|error| anyhow::anyhow!(error))?;
    };
    Ok(())
}

pub async fn run(cfg: Config) -> Result<()> {
    run_until(cfg, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

pub async fn run_until<S>(cfg: Config, shutdown: S) -> Result<()>
where
    S: Future<Output = ()>,
{
    let identity_was_missing = !cfg.key_path.exists();
    let key = identity::load_or_create(&cfg.key_path)?;

    let store = Arc::new(Store::new(cfg.max_registrations));
    let metrics = Arc::new(Metrics::default());
    if identity_was_missing {
        metrics.record_identity_generated();
    }
    metrics::spawn_metrics_endpoint(cfg.metrics_addr, Arc::clone(&metrics), Arc::clone(&store));

    let handler = Arc::new(Handler {
        store: Arc::clone(&store),
        clock_skew: cfg.clock_skew(),
        replay_window: cfg.replay_window(),
    });

    let relay_config = cfg.relay_config();
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(key.clone())
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default().nodelay(true),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )?
        .with_quic()
        .with_behaviour(|key| Behaviour::new_with_relay_config(key, relay_config))
        .map_err(|e| anyhow::anyhow!("behaviour: {e}"))?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    for (name, addr) in listen_addrs(&cfg)? {
        swarm
            .listen_on(addr)
            .with_context(|| format!("listen on {name}"))?;
    }

    if let Some(ext) = &cfg.external_addr {
        let m: Multiaddr = ext.parse().context("parse external_addr")?;
        swarm.add_external_address(m);
    }

    info!(
        peer_id = %swarm.local_peer_id(),
        relay_max_reservations = cfg.relay_max_reservations,
        relay_max_circuits = cfg.relay_max_circuits,
        "rendezvous server up"
    );

    let mut prune_tick = tokio::time::interval(Duration::from_secs(15));
    let mut shutdown = std::pin::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("shutdown signal");
                break;
            }
            _ = prune_tick.tick() => {
                let n = store.prune_expired();
                if n > 0 {
                    info!(evicted = n, live = store.len(), "pruned");
                }
            }
            event = swarm.select_next_some() => {
                handle_swarm_event(event, &handler, &metrics, &mut swarm);
            }
        }
    }
    Ok(())
}

fn listen_addrs(cfg: &Config) -> Result<Vec<(&'static str, Multiaddr)>> {
    let mut addrs = Vec::with_capacity(4);
    addrs.push((
        "listen_quic",
        parse_multiaddr("listen_quic", &cfg.listen_quic)?,
    ));
    addrs.push((
        "listen_tcp",
        parse_multiaddr("listen_tcp", &cfg.listen_tcp)?,
    ));

    if !cfg.disable_ipv6 {
        addrs.push((
            "listen_quic_v6",
            parse_multiaddr("listen_quic_v6", &cfg.listen_quic_v6)?,
        ));
        addrs.push((
            "listen_tcp_v6",
            parse_multiaddr("listen_tcp_v6", &cfg.listen_tcp_v6)?,
        ));
    }

    Ok(addrs)
}

fn parse_multiaddr(name: &'static str, value: &str) -> Result<Multiaddr> {
    value
        .parse()
        .with_context(|| format!("parse {name} {value:?}"))
}

fn handle_swarm_event(
    event: SwarmEvent<behaviour::BehaviourEvent>,
    handler: &Arc<Handler>,
    metrics: &Metrics,
    swarm: &mut libp2p::Swarm<Behaviour>,
) {
    match event {
        SwarmEvent::NewListenAddr { address, .. } => info!(%address, "listening"),
        SwarmEvent::Behaviour(behaviour::BehaviourEvent::Rendezvous(
            request_response::Event::Message { peer, message, .. },
        )) => match message {
            request_response::Message::Request {
                request, channel, ..
            } => {
                let resp = handler.handle(request);
                if swarm
                    .behaviour_mut()
                    .rendezvous
                    .send_response(channel, resp)
                    .is_err()
                {
                    warn!(%peer, "send_response failed (channel closed?)");
                }
            }
            request_response::Message::Response { .. } => {
                // Server doesn't initiate requests.
            }
        },
        SwarmEvent::Behaviour(behaviour::BehaviourEvent::Rendezvous(
            request_response::Event::OutboundFailure { peer, error, .. },
        )) => {
            warn!(%peer, ?error, "outbound rendezvous failure");
        }
        SwarmEvent::Behaviour(behaviour::BehaviourEvent::Rendezvous(
            request_response::Event::InboundFailure { peer, error, .. },
        )) => {
            warn!(%peer, ?error, "inbound rendezvous failure");
        }
        SwarmEvent::Behaviour(behaviour::BehaviourEvent::Relay(event)) => {
            handle_relay_event(event, metrics);
        }
        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
            metrics.record_connection_established();
            tracing::debug!(%peer_id, "connection established");
        }
        SwarmEvent::ConnectionClosed { peer_id, cause, .. } => {
            metrics.record_connection_closed();
            tracing::debug!(%peer_id, ?cause, "connection closed");
        }
        SwarmEvent::IncomingConnectionError { error, .. } => {
            tracing::debug!(?error, "incoming conn error");
        }
        SwarmEvent::OutgoingConnectionError { error, .. } => {
            tracing::debug!(?error, "outgoing conn error");
        }
        other => {
            tracing::trace!(?other, "swarm event");
        }
    }
}

fn handle_relay_event(event: relay::Event, metrics: &Metrics) {
    match event {
        relay::Event::ReservationReqAccepted {
            src_peer_id,
            renewed,
        } => {
            metrics.record_relay_reservation_accepted(renewed);
            info!(%src_peer_id, renewed, "relay reservation accepted");
        }
        relay::Event::ReservationReqDenied {
            src_peer_id,
            status,
        } => {
            metrics.record_relay_reservation_denied();
            warn!(%src_peer_id, ?status, "relay reservation denied");
        }
        relay::Event::ReservationClosed { src_peer_id }
        | relay::Event::ReservationTimedOut { src_peer_id } => {
            metrics.record_relay_reservation_closed();
            info!(%src_peer_id, "relay reservation closed");
        }
        relay::Event::CircuitReqAccepted {
            src_peer_id,
            dst_peer_id,
        } => {
            metrics.record_relay_circuit_accepted();
            info!(%src_peer_id, %dst_peer_id, "relay circuit accepted");
        }
        relay::Event::CircuitReqDenied {
            src_peer_id,
            dst_peer_id,
            status,
        } => {
            metrics.record_relay_circuit_denied();
            warn!(%src_peer_id, %dst_peer_id, ?status, "relay circuit denied");
        }
        relay::Event::CircuitClosed {
            src_peer_id,
            dst_peer_id,
            error,
        } => {
            metrics.record_relay_circuit_closed();
            info!(%src_peer_id, %dst_peer_id, ?error, "relay circuit closed");
        }
        other => tracing::trace!(?other, "relay event"),
    }
}
