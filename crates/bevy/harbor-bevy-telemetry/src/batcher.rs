use diagnostics_policy::QueueAction;
use telemetry_protocol::{Batch, Envelope, EventMetadata};

/// One bounded batch in insertion order. Collection policy is the caller's
/// responsibility; use `SessionRecorder` for consent-gated ingress.
pub struct EnvelopeBatcher<E> {
    pending: Batch<E>,
    limit: usize,
}

impl<E> EnvelopeBatcher<E> {
    pub fn new(batch_size: usize) -> Self {
        Self {
            pending: Batch { events: Vec::new() },
            limit: batch_size.max(1),
        }
    }

    pub fn len(&self) -> usize {
        self.pending.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.events.is_empty()
    }

    pub fn is_full(&self) -> bool {
        self.len() >= self.limit
    }

    /// Returns the event unchanged when the batch is full.
    pub fn push(&mut self, event: E, metadata: EventMetadata) -> Result<(), E> {
        if self.is_full() {
            return Err(event);
        }
        self.pending.events.push(Envelope { metadata, event });
        Ok(())
    }

    pub fn drain(&mut self) -> Option<Batch<E>> {
        (!self.is_empty()).then(|| Batch {
            events: std::mem::take(&mut self.pending.events),
        })
    }

    pub fn withdraw_consent(&mut self) {
        let (_, action) = diagnostics_policy::withdraw();
        self.apply(action);
    }

    pub(crate) fn apply(&mut self, action: QueueAction) {
        if action == QueueAction::ClearPending {
            self.pending.events.clear();
        }
    }

    pub(crate) fn batch(&self) -> &Batch<E> {
        &self.pending
    }
}
