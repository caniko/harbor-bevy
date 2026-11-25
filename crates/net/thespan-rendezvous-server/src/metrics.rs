use crate::store::Store;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{info, warn};

#[derive(Default)]
pub struct Metrics {
    identity_generated_total: AtomicU64,
    connections_established_total: AtomicU64,
    connections_closed_total: AtomicU64,
    relay_reservations_current: AtomicU64,
    relay_reservations_accepted_total: AtomicU64,
    relay_reservations_denied_total: AtomicU64,
    relay_circuits_current: AtomicU64,
    relay_circuits_accepted_total: AtomicU64,
    relay_circuits_denied_total: AtomicU64,
}

impl Metrics {
    pub fn record_identity_generated(&self) {
        self.identity_generated_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_connection_established(&self) {
        self.connections_established_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_connection_closed(&self) {
        self.connections_closed_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_relay_reservation_accepted(&self, renewed: bool) {
        self.relay_reservations_accepted_total
            .fetch_add(1, Ordering::Relaxed);
        if !renewed {
            self.relay_reservations_current
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_relay_reservation_closed(&self) {
        decrement_saturating(&self.relay_reservations_current);
    }

    pub fn record_relay_reservation_denied(&self) {
        self.relay_reservations_denied_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_relay_circuit_accepted(&self) {
        self.relay_circuits_accepted_total
            .fetch_add(1, Ordering::Relaxed);
        self.relay_circuits_current.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_relay_circuit_closed(&self) {
        decrement_saturating(&self.relay_circuits_current);
    }

    pub fn record_relay_circuit_denied(&self) {
        self.relay_circuits_denied_total
            .fetch_add(1, Ordering::Relaxed);
    }

    fn render(&self, live_registrations: usize) -> String {
        format!(
            concat!(
                "# TYPE thespan_rendezvous_identity_generated_total counter\n",
                "thespan_rendezvous_identity_generated_total {}\n",
                "# TYPE thespan_rendezvous_connections_established_total counter\n",
                "thespan_rendezvous_connections_established_total {}\n",
                "# TYPE thespan_rendezvous_connections_closed_total counter\n",
                "thespan_rendezvous_connections_closed_total {}\n",
                "# TYPE thespan_rendezvous_registrations gauge\n",
                "thespan_rendezvous_registrations {}\n",
                "# TYPE thespan_rendezvous_relay_reservations_current gauge\n",
                "thespan_rendezvous_relay_reservations_current {}\n",
                "# TYPE thespan_rendezvous_relay_reservations_accepted_total counter\n",
                "thespan_rendezvous_relay_reservations_accepted_total {}\n",
                "# TYPE thespan_rendezvous_relay_reservations_denied_total counter\n",
                "thespan_rendezvous_relay_reservations_denied_total {}\n",
                "# TYPE thespan_rendezvous_relay_circuits_current gauge\n",
                "thespan_rendezvous_relay_circuits_current {}\n",
                "# TYPE thespan_rendezvous_relay_circuits_accepted_total counter\n",
                "thespan_rendezvous_relay_circuits_accepted_total {}\n",
                "# TYPE thespan_rendezvous_relay_circuits_denied_total counter\n",
                "thespan_rendezvous_relay_circuits_denied_total {}\n"
            ),
            self.identity_generated_total.load(Ordering::Relaxed),
            self.connections_established_total.load(Ordering::Relaxed),
            self.connections_closed_total.load(Ordering::Relaxed),
            live_registrations,
            self.relay_reservations_current.load(Ordering::Relaxed),
            self.relay_reservations_accepted_total
                .load(Ordering::Relaxed),
            self.relay_reservations_denied_total.load(Ordering::Relaxed),
            self.relay_circuits_current.load(Ordering::Relaxed),
            self.relay_circuits_accepted_total.load(Ordering::Relaxed),
            self.relay_circuits_denied_total.load(Ordering::Relaxed),
        )
    }
}

pub fn spawn_metrics_endpoint(addr: SocketAddr, metrics: Arc<Metrics>, store: Arc<Store>) {
    tokio::spawn(async move {
        if let Err(error) = serve_metrics(addr, metrics, store).await {
            warn!(%addr, ?error, "metrics endpoint stopped");
        }
    });
}

async fn serve_metrics(
    addr: SocketAddr,
    metrics: Arc<Metrics>,
    store: Arc<Store>,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    info!(%addr, "metrics endpoint listening");

    loop {
        let (mut stream, _) = listener.accept().await?;
        let metrics = Arc::clone(&metrics);
        let store = Arc::clone(&store);
        tokio::spawn(async move {
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await;

            let body = metrics.render(store.len());
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/plain; version=0.0.4\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            if let Err(error) = stream.write_all(response.as_bytes()).await {
                warn!(?error, "metrics response write failed");
            }
        });
    }
}

fn decrement_saturating(counter: &AtomicU64) {
    let mut current = counter.load(Ordering::Relaxed);
    while current > 0 {
        match counter.compare_exchange_weak(
            current,
            current - 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(next) => current = next,
        }
    }
}
