//! The network actor: drives the libp2p Swarm as a piying Actor.
//!
//! Communicates with the consumer via:
//! - piying mailbox: receives `NetworkCommand` messages
//! - `event_tx`: pushes `ActorEvent` to consumer via crossbeam

include!("network_parts/actor_runtime.rs");
include!("network_parts/retry_and_interfaces.rs");
