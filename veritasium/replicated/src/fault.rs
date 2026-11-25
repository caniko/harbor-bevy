//! Fault-injection registry for transport chokepoints.
//!
//! Test harnesses schedule per-frame faults against named operations;
//! production hot paths call [`probe`] at each chokepoint and act on the
//! returned [`FaultKind`]. When no registry is installed (the normal,
//! non-test case) a probe is one thread-local read.
//!
//! The message-kind discriminant in [`OpId::Publish`] is the consumer's
//! stable wire tag (e.g. a `#[repr(u16)]` enum discriminant) so this
//! module needs no knowledge of the game's kind enum.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// A fault-injectable operation site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OpId {
    /// Outbound broadcast publish of a message kind (wire tag).
    Publish { kind: u16 },
    /// Transport connection drop.
    ConnectionDrop,
    /// Outbound send via a reliability shim.
    ShimSend,
    /// Inbound receive via session actor dispatch.
    ShimRecv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    PublishFailed,
    Timeout,
    Disconnect {
        after_bytes: usize,
    },
    /// Silently drop the outbound message (no send).
    DropOutbound,
    /// Silently drop the inbound message (no processing).
    DropInbound,
    /// Deliver N frames later than normal.
    Delay {
        frames: u32,
    },
    /// Hold a window of messages and release them in random order.
    Reorder {
        window: usize,
    },
    /// Deliver N extra copies after the original.
    Duplicate {
        count: u32,
    },
    /// Drop all messages to/from a peer for N frames.
    BlackholePeer {
        frames: u64,
    },
    /// Fail the send with an error visible to the caller (unlike DropOutbound,
    /// which pretends success). Exercises the SendFailed error path.
    FailSend,
}

#[derive(Debug, Default, Clone)]
pub struct FaultRegistry {
    schedule: BTreeMap<(u64, OpId), FaultKind>,
    current_frame: u64,
}

impl FaultRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn schedule(&mut self, frame: u64, op: OpId, kind: FaultKind) {
        self.schedule.insert((frame, op), kind);
    }

    pub fn set_frame(&mut self, frame: u64) {
        self.current_frame = frame;
    }

    pub fn check(&self, op: &OpId) -> Option<FaultKind> {
        self.schedule.get(&(self.current_frame, *op)).copied()
    }
}

thread_local! {
    static REGISTRY: RefCell<Option<FaultRegistry>> = const { RefCell::new(None) };
}

static PROBE_HIT_COUNT: AtomicU64 = AtomicU64::new(0);

pub fn install_registry(registry: FaultRegistry) -> Option<FaultRegistry> {
    REGISTRY.with(|cell| cell.borrow_mut().replace(registry))
}

pub fn uninstall_registry() -> Option<FaultRegistry> {
    REGISTRY.with(|cell| cell.borrow_mut().take())
}

pub fn set_active_frame(frame: u64) {
    REGISTRY.with(|cell| {
        if let Some(registry) = cell.borrow_mut().as_mut() {
            registry.set_frame(frame);
        }
    });
}

pub fn current_frame() -> u64 {
    REGISTRY.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|registry| registry.current_frame)
            .unwrap_or(0)
    })
}

#[inline]
pub fn probe(op: OpId) -> Result<(), FaultKind> {
    let scheduled = REGISTRY.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|registry| registry.check(&op))
    });
    match scheduled {
        Some(kind) => Err(kind),
        None => {
            PROBE_HIT_COUNT.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }
}

pub fn probe_hit_count() -> u64 {
    PROBE_HIT_COUNT.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_probe_returns_scheduled_fault() {
        let mut registry = FaultRegistry::new();
        registry.schedule(4, OpId::Publish { kind: 0x21 }, FaultKind::PublishFailed);
        registry.set_frame(4);
        install_registry(registry);

        assert_eq!(
            probe(OpId::Publish { kind: 0x21 }),
            Err(FaultKind::PublishFailed)
        );
        uninstall_registry();
    }
}
