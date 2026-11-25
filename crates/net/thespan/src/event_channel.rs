//! Bounded event lanes. Producers never wait for a consumer to free capacity.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossbeam_channel::{RecvTimeoutError, TryRecvError};
use parking_lot::{Condvar, Mutex};

use crate::actor::events::{ActorErrorKind, ActorEvent};
use crate::handle::{THESPAN_EVENT_DROPS, THESPAN_RELIABLE_OVERFLOWS};

/// Single producer owned by the network actor.
pub struct EventSender {
    shared: Arc<Shared>,
    capacity: usize,
    overflowed: bool,
}

/// Receiver for reliable events and best-effort telemetry, in that priority order.
pub struct EventReceiver(Arc<Shared>);

#[derive(Default)]
struct Shared {
    queues: Mutex<Queues>,
    ready: Condvar,
}

#[derive(Default)]
struct Queues {
    reliable: VecDeque<ActorEvent>,
    telemetry: VecDeque<ActorEvent>,
    closed: bool,
}

impl Queues {
    fn pop(&mut self) -> Result<ActorEvent, TryRecvError> {
        self.reliable
            .pop_front()
            .or_else(|| self.telemetry.pop_front())
            .ok_or(if self.closed {
                TryRecvError::Disconnected
            } else {
                TryRecvError::Empty
            })
    }
}

/// Create an event channel with the production capacity per lane.
pub fn event_channel() -> (EventSender, EventReceiver) {
    channel(crate::spawn::THESPAN_EVENT_CHANNEL_CAPACITY)
}

fn channel(capacity: usize) -> (EventSender, EventReceiver) {
    let shared = Arc::new(Shared::default());
    (
        EventSender {
            shared: shared.clone(),
            capacity,
            overflowed: false,
        },
        EventReceiver(shared),
    )
}

impl EventSender {
    pub(crate) fn overflowed(&self) -> bool {
        self.overflowed
    }

    pub(crate) fn push(&mut self, event: ActorEvent) {
        let mut queues = self.shared.queues.lock();
        if queues.closed {
            return;
        }

        if matches!(
            event,
            ActorEvent::GossipReceived { .. } | ActorEvent::OversizedMessageDropped { .. }
        ) {
            if queues.telemetry.len() == self.capacity {
                queues.telemetry.pop_front();
                THESPAN_EVENT_DROPS.fetch_add(1, Ordering::Relaxed);
            }
            queues.telemetry.push_back(event);
        } else if queues.reliable.len() == self.capacity {
            self.overflowed = true;
            THESPAN_RELIABLE_OVERFLOWS.fetch_add(1, Ordering::Relaxed);
            // One reserved slot guarantees the terminal error is observable,
            // after the already queued reliable events, even at capacity.
            tracing::error!(
                target: "thespan::backpressure",
                capacity = self.capacity,
                "reliable event queue full; stopping actor"
            );
            queues.reliable.push_back(ActorEvent::Error {
                kind: ActorErrorKind::ReliableEventOverflow,
                error: format!(
                    "reliable event queue full ({}); stopping actor",
                    self.capacity
                ),
            });
            queues.telemetry.clear();
            queues.closed = true;
        } else {
            queues.reliable.push_back(event);
        }
        drop(queues);
        self.shared.ready.notify_all();
    }
}

impl Drop for EventSender {
    fn drop(&mut self) {
        self.shared.queues.lock().closed = true;
        self.shared.ready.notify_all();
    }
}

impl EventReceiver {
    /// Receive the next event without waiting.
    pub fn try_recv(&self) -> Result<ActorEvent, TryRecvError> {
        self.0.queues.lock().pop()
    }

    /// Wait up to `timeout` for the next event.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<ActorEvent, RecvTimeoutError> {
        let start = Instant::now();
        let mut queues = self.0.queues.lock();
        loop {
            match queues.pop() {
                Ok(event) => return Ok(event),
                Err(TryRecvError::Disconnected) => return Err(RecvTimeoutError::Disconnected),
                Err(TryRecvError::Empty) => {}
            }
            let Some(remaining) = timeout
                .checked_sub(start.elapsed())
                .filter(|d| !d.is_zero())
            else {
                return Err(RecvTimeoutError::Timeout);
            };
            self.0.ready.wait_for(&mut queues, remaining);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gossip(value: u8) -> ActorEvent {
        ActorEvent::GossipReceived {
            topic: "updates".into(),
            source: None,
            payload: vec![value],
        }
    }

    #[test]
    fn telemetry_evicts_only_the_oldest_telemetry() {
        let (mut tx, rx) = channel(2);
        tx.push(gossip(1));
        tx.push(gossip(2));
        tx.push(ActorEvent::ListenStarted {
            addr: "/memory/1".parse().unwrap(),
        });
        let before = crate::NetworkHandle::event_drops_total();
        tx.push(gossip(3));
        assert!(crate::NetworkHandle::event_drops_total() > before);
        assert!(matches!(
            rx.try_recv(),
            Ok(ActorEvent::ListenStarted { .. })
        ));
        for expected in [2, 3] {
            assert!(matches!(
                rx.try_recv(),
                Ok(ActorEvent::GossipReceived { payload, .. }) if payload == [expected]
            ));
        }
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[tokio::test]
    async fn reliable_overflow_stops_actor_and_disconnects_peer() {
        use crate::actor::network::{NetworkActor, NetworkActorArgs};
        use crate::transport::lan::build_lan_swarm_with_keypair;
        use crate::{CommandHandle, LanConfig, NetworkCommand, ThespanConfig, identity};
        use futures::StreamExt;
        use libp2p::swarm::SwarmEvent;
        use piying::actor::Spawn;

        const CAPACITY: usize = 2;
        const TIMEOUT: Duration = Duration::from_secs(5);

        let build = || {
            build_lan_swarm_with_keypair(
                &LanConfig::default(),
                identity::Keypair::generate_ed25519(),
                &ThespanConfig::default(),
            )
            .unwrap()
        };
        let (event_tx, rx) = channel(CAPACITY);
        let prepared = NetworkActor::prepare_with_mailbox(piying::mailbox::bounded(8));
        let command = CommandHandle::from_piying(prepared.actor_ref().clone());
        let task = tokio::spawn(prepared.run(NetworkActorArgs {
            swarm: build(),
            listen_addr: "/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap(),
            additional_listen_addrs: vec![],
            event_tx,
            bootstrap_tx: None,
            infrastructure_peers: Default::default(),
            dial_addr: None,
        }));

        let (addr, rx) = tokio::task::spawn_blocking(move || {
            let ActorEvent::ListenStarted { addr } = rx.recv_timeout(TIMEOUT).unwrap() else {
                panic!("actor did not announce its listener");
            };
            (addr, rx)
        })
        .await
        .unwrap();
        let mut remote = build();
        remote.dial(addr).unwrap();

        tokio::time::timeout(TIMEOUT, async {
            while !matches!(
                remote.select_next_some().await,
                SwarmEvent::ConnectionEstablished { .. }
            ) {}
            let rx = tokio::task::spawn_blocking(move || {
                assert!(matches!(
                    rx.recv_timeout(TIMEOUT),
                    Ok(ActorEvent::PeerConnected { .. })
                ));
                rx
            })
            .await
            .unwrap();

            let before = crate::NetworkHandle::reliable_overflows_total();
            for _ in 0..=CAPACITY {
                // With gossip disabled, each subscription produces a reliable failure event.
                command
                    .send(NetworkCommand::Subscribe {
                        topic: "updates".into(),
                    })
                    .unwrap();
            }
            // Drop the returned actor too: in production the runtime does so immediately.
            drop(task.await.unwrap().unwrap());
            while !matches!(
                remote.select_next_some().await,
                SwarmEvent::ConnectionClosed { .. }
            ) {}

            assert!(crate::NetworkHandle::reliable_overflows_total() > before);
            for _ in 0..CAPACITY {
                assert!(matches!(
                    rx.try_recv(),
                    Ok(ActorEvent::GossipSubscribeFailed { .. })
                ));
            }
            assert!(matches!(
                rx.try_recv(),
                Ok(ActorEvent::Error {
                    kind: ActorErrorKind::ReliableEventOverflow,
                    ..
                })
            ));
            assert!(matches!(rx.try_recv(), Err(TryRecvError::Disconnected)));
        })
        .await
        .expect("actor should stop and close the peer connection");
    }
}
