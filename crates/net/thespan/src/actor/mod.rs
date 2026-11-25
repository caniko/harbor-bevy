//! Network actor module: drives a libp2p Swarm in a background thread.

pub mod behavior;
pub mod commands;
pub mod events;
pub mod network;

pub use commands::NetworkCommand;
pub use events::ActorEvent;
pub use network::NetworkActor;
