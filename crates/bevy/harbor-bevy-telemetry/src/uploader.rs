use std::{
    collections::VecDeque,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

use bevy::{log::warn, prelude::Resource};
use rkyv::{
    Serialize, api::high::HighSerializer, rancor::Error as ArchiveError,
    ser::allocator::ArenaHandle, util::AlignedVec,
};
use telemetry_client::{ClientError, TelemetryClient};
use telemetry_protocol::Batch;
use tokio::{runtime::Handle, sync::watch, task::AbortHandle};

use crate::TelemetryConfig;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UploadOutcome {
    Sent,
    /// The endpoint refused authorization. This uploader will not send again.
    Refused,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum UploadError {
    #[error("upload failed transiently")]
    Retryable,
    #[error("upload failed permanently")]
    Permanent,
    #[error("telemetry collection is not allowed")]
    NotAllowed,
    #[error("telemetry upload authorization was refused")]
    Refused,
    #[error("pending upload queue is full")]
    QueueFull,
    #[error("telemetry upload worker has stopped")]
    WorkerStopped,
}

/// A transport should keep its work in the returned future so withdrawal can
/// cancel it by dropping the future. Retries may deliver a batch more than once.
pub trait TelemetryTransport<E>: Send + Sync + 'static {
    fn send_batch(
        &self,
        batch: &Batch<E>,
    ) -> impl Future<Output = Result<UploadOutcome, UploadError>> + Send;
}

#[derive(Clone)]
pub struct TelemetryClientTransport(pub TelemetryClient);

impl<E: Send + Sync + 'static> TelemetryTransport<E> for TelemetryClientTransport
where
    Batch<E>: for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, ArchiveError>>,
{
    async fn send_batch(&self, batch: &Batch<E>) -> Result<UploadOutcome, UploadError> {
        match self.0.send_batch(batch).await {
            Ok(()) => Ok(UploadOutcome::Sent),
            Err(ClientError::Status(status)) if matches!(status.as_u16(), 401 | 403) => {
                Ok(UploadOutcome::Refused)
            }
            Err(ClientError::Status(status))
                if status.is_server_error() || matches!(status.as_u16(), 408 | 429) =>
            {
                Err(UploadError::Retryable)
            }
            Err(ClientError::Transport(_)) => Err(UploadError::Retryable),
            Err(_) => Err(UploadError::Permanent),
        }
    }
}

// ponytail: one in-flight batch and eight queued batches; add a durable outbox
// only if the application needs delivery guarantees across outages/restarts.
const QUEUE_CAPACITY: usize = 8;
const MAX_BACKOFF_SECS: u64 = 3_600;

#[derive(Clone, Copy, Eq, PartialEq)]
struct Policy {
    allowed: bool,
    max_retries: u32,
    base_secs: u64,
}

impl From<&TelemetryConfig> for Policy {
    fn from(config: &TelemetryConfig) -> Self {
        Self {
            allowed: config.may_collect(),
            max_retries: config.max_retries,
            base_secs: config.retry_backoff_base_secs,
        }
    }
}

struct State<E> {
    policy: Policy,
    generation: u64,
    refused: bool,
    /// The first `ready` queued batches may be sent; the rest await a tick.
    ready: usize,
    queue: VecDeque<Batch<E>>,
}

impl<E> State<E> {
    fn clear(&mut self) {
        self.ready = 0;
        self.queue.clear();
    }
}

/// Best-effort background uploads on a caller-owned Tokio runtime with time
/// enabled. Drop this resource to stop its worker. Replace it with a new
/// uploader after resolving an authorization refusal.
#[derive(Resource)]
pub struct TelemetryUploader<E: Send + Sync + 'static> {
    state: Arc<Mutex<State<E>>>,
    wake: watch::Sender<u64>,
    worker: AbortHandle,
}

impl<E: Send + Sync + 'static> TelemetryUploader<E> {
    pub fn new(
        runtime: &Handle,
        config: &TelemetryConfig,
        transport: impl TelemetryTransport<E>,
    ) -> Self {
        let state = Arc::new(Mutex::new(State {
            policy: config.into(),
            generation: 0,
            refused: false,
            ready: 0,
            queue: VecDeque::new(),
        }));
        let (wake, receiver) = watch::channel(0);
        let worker = runtime
            .spawn(run(Arc::clone(&state), receiver, transport))
            .abort_handle();
        Self {
            state,
            wake,
            worker,
        }
    }

    /// Apply a new policy synchronously, clearing queued batches and waking
    /// the worker to cancel any in-flight attempt or backoff under the old one.
    pub fn update_config(&self, config: &TelemetryConfig) {
        let policy = Policy::from(config);
        let mut state = self.state.lock().expect("uploader state lock poisoned");
        if state.policy != policy {
            state.policy = policy;
            state.generation = state.generation.wrapping_add(1);
            self.wake.send_replace(state.generation);
            state.clear();
        }
    }

    pub fn is_refused(&self) -> bool {
        self.state
            .lock()
            .expect("uploader state lock poisoned")
            .refused
    }

    /// Number of ready and staged batches, excluding the in-flight batch.
    pub fn pending_len(&self) -> usize {
        self.state
            .lock()
            .expect("uploader state lock poisoned")
            .queue
            .len()
    }

    /// Submit a batch for immediate background delivery.
    pub fn queue_batch(&self, batch: Batch<E>) -> Result<(), UploadError> {
        self.enqueue(batch, true)
    }

    pub(crate) fn stage_batch(&self, batch: Batch<E>) -> Result<(), UploadError> {
        self.enqueue(batch, false)
    }

    fn enqueue(&self, batch: Batch<E>, ready: bool) -> Result<(), UploadError> {
        let mut state = self.state.lock().expect("uploader state lock poisoned");
        if state.refused {
            return Err(UploadError::Refused);
        }
        if !state.policy.allowed {
            return Err(UploadError::NotAllowed);
        }
        if self.worker.is_finished() {
            return Err(UploadError::WorkerStopped);
        }
        if batch.events.is_empty() {
            return Ok(());
        }
        if state.queue.len() >= QUEUE_CAPACITY {
            return Err(UploadError::QueueFull);
        }
        if ready {
            let position = state.ready;
            state.queue.insert(position, batch);
            state.ready += 1;
            self.wake.send_replace(state.generation);
        } else {
            state.queue.push_back(batch);
        }
        Ok(())
    }

    pub(crate) fn release_staged(&self) {
        let mut state = self.state.lock().expect("uploader state lock poisoned");
        if state.ready != state.queue.len() {
            state.ready = state.queue.len();
            self.wake.send_replace(state.generation);
        }
    }
}

impl<E: Send + Sync + 'static> Drop for TelemetryUploader<E> {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

async fn run<E: Send + Sync + 'static>(
    state: Arc<Mutex<State<E>>>,
    mut wake: watch::Receiver<u64>,
    transport: impl TelemetryTransport<E>,
) {
    loop {
        let work = {
            let mut state = state.lock().expect("uploader state lock poisoned");
            if state.ready == 0 {
                None
            } else {
                state.ready -= 1;
                state
                    .queue
                    .pop_front()
                    .map(|batch| (batch, state.generation, state.policy))
            }
        };
        let Some((batch, generation, policy)) = work else {
            if wake.changed().await.is_err() {
                break;
            }
            continue;
        };
        let result = tokio::select! {
            biased;
            // Wake and drop the running future on revocation, even during
            // backoff. New work also wakes this channel, but keeps the generation.
            _ = wake.wait_for(|live| *live != generation) => continue,
            result = send_with_retry(&transport, &batch, policy) => result,
        };
        match result {
            Ok(UploadOutcome::Sent) => {}
            Ok(UploadOutcome::Refused) | Err(UploadError::Refused) => {
                let mut state = state.lock().expect("uploader state lock poisoned");
                // Policy can change between receiving a refusal and taking
                // this lock, but it cannot replace this worker's transport.
                state.refused = true;
                state.clear();
            }
            Err(error) => warn!("{error}"),
        }
    }
}

async fn send_with_retry<E>(
    transport: &impl TelemetryTransport<E>,
    batch: &Batch<E>,
    policy: Policy,
) -> Result<UploadOutcome, UploadError> {
    let mut delay = policy.base_secs.min(MAX_BACKOFF_SECS);
    let mut retries = 0;
    loop {
        match transport.send_batch(batch).await {
            Err(UploadError::Retryable) if retries < policy.max_retries => {
                retries += 1;
                tokio::time::sleep(Duration::from_secs(delay)).await;
                delay = delay.saturating_mul(2).min(MAX_BACKOFF_SECS);
            }
            result => return result,
        }
    }
}
