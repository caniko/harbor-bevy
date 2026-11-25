//! Engine-independent rkyv types. Applications define and version their own
//! event payloads and agree on the wire schema with their servers.

use rkyv::{Archive, Deserialize, Serialize};

/// Milliseconds since the Unix epoch.
#[derive(Archive, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timestamp(pub u64);

/// Caller-supplied, opaque telemetry session identifier.
#[derive(Archive, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SessionId(pub String);

#[derive(Archive, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ClientHeader {
    pub name: String,
    pub version: String,
}

#[derive(Archive, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct EventMetadata {
    pub timestamp: Timestamp,
    pub session: SessionId,
    pub client: ClientHeader,
}

#[derive(Archive, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Envelope<E> {
    pub metadata: EventMetadata,
    pub event: E,
}

#[derive(Archive, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Batch<E> {
    pub events: Vec<Envelope<E>>,
}

#[cfg(test)]
mod tests;
