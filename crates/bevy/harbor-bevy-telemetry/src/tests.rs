use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

use bevy::{app::AppExit, prelude::App};
use diagnostics_policy::{Consent, ProfileKind, RetentionCap};
use telemetry_protocol::{Batch, ClientHeader, EventMetadata, SessionId, Timestamp};
use tokio::{runtime::Handle, sync::mpsc, time::Instant};
use uuid::Uuid;

use super::*;

fn config() -> TelemetryConfig {
    TelemetryConfig {
        profile: ProfileKind::Adult,
        consent: Consent::Granted,
        client: ClientHeader {
            name: "example".into(),
            version: "1".into(),
        },
        batch_size: 2,
        ..Default::default()
    }
}

fn session() -> SessionId {
    SessionId("../opaque/session".into())
}

fn metadata() -> EventMetadata {
    EventMetadata {
        timestamp: Timestamp(42),
        session: session(),
        client: config().client,
    }
}

fn batch(event: u32) -> Batch<u32> {
    let mut batcher = EnvelopeBatcher::new(1);
    batcher.push(event, metadata()).unwrap();
    batcher.drain().unwrap()
}

#[test]
fn batcher_preserves_order_and_metadata_and_enforces_size() {
    let mut batcher = EnvelopeBatcher::new(2);
    assert!(batcher.drain().is_none());
    batcher.push(7_u32, metadata()).unwrap();
    batcher.push(9, metadata()).unwrap();
    assert_eq!(batcher.push(11, metadata()), Err(11));
    let batch = batcher.drain().unwrap();
    assert_eq!(
        batch.events.iter().map(|e| e.event).collect::<Vec<_>>(),
        [7, 9]
    );
    assert!(batch.events.iter().all(|e| e.metadata == metadata()));
    batcher.push(13, metadata()).unwrap();
    batcher.withdraw_consent();
    assert!(batcher.drain().is_none());
}

#[test]
fn recorder_checks_live_policy_at_ingress_without_spending_sequence_numbers() {
    let mut config = config();
    let mut recorder = SessionRecorder::new(session(), &config);
    for consent in [Consent::Unknown, Consent::Denied, Consent::Withdrawn] {
        config.consent = consent;
        assert_eq!(
            recorder.record(&config, 1_u32),
            Err(RecordError::NotAllowed)
        );
    }
    config.consent = Consent::Granted;
    config.profile = ProfileKind::Child;
    assert_eq!(recorder.record(&config, 2), Err(RecordError::NotAllowed));
    config.profile = ProfileKind::Adult;
    assert_eq!(recorder.record(&config, 3), Ok(0));
    assert_eq!(recorder.record(&config, 4), Ok(1));
    assert_eq!(recorder.record(&config, 5), Err(RecordError::BatchFull));
    let batch = recorder.flush_batch(&config).unwrap().unwrap();
    assert_eq!(batch.events[0].metadata.session, session());
    assert_eq!(batch.events[1].event, 4);
    assert_eq!(recorder.record(&config, 6), Ok(2));
    recorder.withdraw_consent(&mut config, None, false).unwrap();
    assert_eq!(config.consent, Consent::Withdrawn);
    config.consent = Consent::Granted;
    assert!(recorder.flush_batch(&config).unwrap().is_none());
    assert_eq!(recorder.record(&config, 7), Ok(3));
}

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(Uuid::new_v4().to_string());
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn storage_round_trips_and_cleanup_only_removes_owned_expired_files() {
    let dir = TestDir::new();
    let mut config = TelemetryConfig {
        storage_dir: Some(dir.0.clone()),
        retention: RetentionCap {
            log_days: 1,
            ..Default::default()
        },
        ..config()
    };
    let mut recorder = SessionRecorder::new(session(), &config);
    recorder.record(&config, 10_u32).unwrap();
    recorder.flush_batch(&config).unwrap().unwrap();
    let old = std::fs::read_dir(&dir.0)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let bytes = std::fs::read(&old).unwrap();
    let mut aligned = rkyv::util::AlignedVec::<16>::new();
    aligned.extend_from_slice(&bytes);
    let archived = rkyv::from_bytes::<Batch<u32>, rkyv::rancor::Error>(&aligned).unwrap();
    assert_eq!(archived.events[0].event, 10);
    assert_eq!(archived.events[0].metadata.session, session());

    let now = SystemTime::now();
    let past = now - Duration::from_secs(2 * 86_400);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(past)
        .unwrap();
    let unrelated = dir.0.join("other.rkyv");
    std::fs::File::create(&unrelated)
        .unwrap()
        .set_modified(past)
        .unwrap();
    recorder.record(&config, 20).unwrap();
    recorder.flush_batch(&config).unwrap().unwrap();
    assert_eq!(cleanup_expired(&config, now).unwrap(), 1);
    assert!(!old.exists());
    assert!(unrelated.exists());
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 2);

    recorder.record(&config, 30).unwrap();
    recorder.withdraw_consent(&mut config, None, true).unwrap();
    assert_eq!(recorder.pending_len(), 0);
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    assert!(unrelated.exists());
}

#[test]
fn failed_storage_flush_keeps_the_pending_batch() {
    let dir = TestDir::new();
    let path = dir.0.join("not-a-directory");
    std::fs::File::create(&path).unwrap();
    let mut config = TelemetryConfig {
        storage_dir: Some(path),
        ..config()
    };
    let mut recorder = SessionRecorder::new(session(), &config);
    recorder.record(&config, 17_u32).unwrap();
    assert!(recorder.flush_batch(&config).is_err());
    config.storage_dir = None;
    assert_eq!(
        recorder.flush_batch(&config).unwrap().unwrap().events[0].event,
        17
    );
}

#[cfg(unix)]
#[test]
fn local_deletion_does_not_follow_telemetry_shaped_symlinks() {
    let dir = TestDir::new();
    let target = dir.0.join("unrelated");
    std::fs::write(&target, b"keep").unwrap();
    let link = dir.0.join(format!("telemetry-{}.rkyv", Uuid::new_v4()));
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let mut config = TelemetryConfig {
        storage_dir: Some(dir.0.clone()),
        ..config()
    };
    let mut recorder = SessionRecorder::<u32>::new(session(), &config);
    recorder.withdraw_consent(&mut config, None, true).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"keep");
    assert!(link.is_symlink());
}

#[test]
fn app_exit_flushes_a_partial_batch() {
    let dir = TestDir::new();
    let config = TelemetryConfig {
        storage_dir: Some(dir.0.clone()),
        ..config()
    };
    let mut app = App::new();
    app.insert_resource(config.clone())
        .add_plugins(TelemetryPlugin::<u32>::new(session()));
    app.world_mut()
        .resource_mut::<SessionRecorder<u32>>()
        .record(&config, 7)
        .unwrap();
    app.world_mut().write_message(AppExit::Success);
    app.update();
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
}

#[test]
fn exit_flush_persists_events_recorded_in_last() {
    use bevy::prelude::*;

    let dir = TestDir::new();
    let config = TelemetryConfig {
        storage_dir: Some(dir.0.clone()),
        ..config()
    };
    let mut app = App::new();
    app.insert_resource(config)
        .add_plugins(TelemetryPlugin::<u32>::new(session()))
        .add_systems(
            Last,
            (|mut exit: MessageReader<AppExit>,
              config: Res<TelemetryConfig>,
              mut recorder: ResMut<SessionRecorder<u32>>| {
                if exit.read().count() != 0 {
                    recorder.record(&config, 42).unwrap();
                }
            })
            .before(TelemetrySystems::ExitFlush),
        );

    app.update();
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
    app.world_mut().write_message(AppExit::Success);
    app.update();

    let paths: Vec<_> = std::fs::read_dir(&dir.0).unwrap().collect();
    let [archive] = paths.as_slice() else {
        panic!("expected exactly one archive");
    };
    let bytes = std::fs::read(archive.as_ref().unwrap().path()).unwrap();
    let mut aligned = rkyv::util::AlignedVec::<16>::new();
    aligned.extend_from_slice(&bytes);
    let batch = rkyv::from_bytes::<Batch<u32>, rkyv::rancor::Error>(&aligned).unwrap();
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.events[0].event, 42);
    assert_eq!(app.should_exit(), Some(AppExit::Success));
}

#[derive(Clone, Copy)]
enum Reply {
    Retry,
    Sent,
    Refused,
    Pending,
}

struct MockTransport {
    replies: Vec<Reply>,
    calls: Arc<AtomicUsize>,
    started: mpsc::UnboundedSender<(u32, Instant)>,
    cancellations: Arc<AtomicUsize>,
}

struct Cancellation(Arc<AtomicUsize>);

impl Drop for Cancellation {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

impl TelemetryTransport<u32> for MockTransport {
    async fn send_batch(&self, batch: &Batch<u32>) -> Result<UploadOutcome, UploadError> {
        let attempt = self.calls.fetch_add(1, Ordering::Relaxed);
        self.started
            .send((batch.events[0].event, Instant::now()))
            .unwrap();
        match self.replies.get(attempt).copied().unwrap_or(Reply::Sent) {
            Reply::Retry => Err(UploadError::Retryable),
            Reply::Sent => Ok(UploadOutcome::Sent),
            Reply::Refused => Ok(UploadOutcome::Refused),
            Reply::Pending => {
                let _cancelled = Cancellation(self.cancellations.clone());
                std::future::pending().await
            }
        }
    }
}

fn mock(replies: Vec<Reply>) -> (MockTransport, mpsc::UnboundedReceiver<(u32, Instant)>) {
    let (started, receiver) = mpsc::unbounded_channel();
    (
        MockTransport {
            replies,
            calls: Arc::default(),
            started,
            cancellations: Arc::default(),
        },
        receiver,
    )
}

async fn next_attempt(started: &mut mpsc::UnboundedReceiver<(u32, Instant)>) -> (u32, Instant) {
    tokio::time::timeout(Duration::from_secs(5), started.recv())
        .await
        .expect("upload attempt timed out")
        .expect("transport dropped")
}

#[tokio::test(start_paused = true)]
async fn periodic_flush_stages_a_batch_until_the_upload_timer_is_due() {
    let (mock, mut started) = mock(Vec::new());
    let config = TelemetryConfig {
        flush_interval_secs: 0,
        upload_interval_secs: u64::MAX,
        ..config()
    };
    let uploader = TelemetryUploader::new(&Handle::current(), &config, mock);
    let mut app = App::new();
    app.insert_resource(config.clone())
        .insert_resource(uploader)
        .add_plugins(TelemetryPlugin::<u32>::new(session()));
    app.world_mut()
        .resource_mut::<SessionRecorder<u32>>()
        .record(&config, 7)
        .unwrap();
    app.update();
    assert_eq!(
        app.world().resource::<SessionRecorder<u32>>().pending_len(),
        0
    );
    assert_eq!(
        app.world()
            .resource::<TelemetryUploader<u32>>()
            .pending_len(),
        1
    );
    assert!(started.try_recv().is_err());
    app.world_mut()
        .resource_mut::<TelemetryConfig>()
        .upload_interval_secs = 0;
    app.update();
    assert_eq!(next_attempt(&mut started).await.0, 7);
}

#[tokio::test(start_paused = true)]
async fn retries_are_exponential_and_exhaust_before_next_batch() {
    let (mock, mut started) = mock(vec![Reply::Retry; 3]);
    let config = TelemetryConfig {
        max_retries: 2,
        retry_backoff_base_secs: 1,
        ..config()
    };
    let uploader = TelemetryUploader::new(&Handle::current(), &config, mock);
    uploader.queue_batch(batch(1)).unwrap();
    uploader.queue_batch(batch(2)).unwrap();
    let (first, a) = next_attempt(&mut started).await;
    let (second, b) = next_attempt(&mut started).await;
    let (third, c) = next_attempt(&mut started).await;
    assert_eq!([first, second, third], [1; 3]);
    for (elapsed, delay) in [
        (b - a, Duration::from_secs(1)),
        (c - b, Duration::from_secs(2)),
    ] {
        assert!((delay..=delay + Duration::from_millis(1)).contains(&elapsed));
    }
    assert_eq!(next_attempt(&mut started).await.0, 2);
}

#[tokio::test(start_paused = true)]
async fn refusal_halts_and_clears_ready_and_staged_uploads() {
    let (mock, mut started) = mock(vec![Reply::Refused]);
    let config = config();
    let uploader = TelemetryUploader::new(&Handle::current(), &config, mock);
    uploader.queue_batch(batch(1)).unwrap();
    uploader.queue_batch(batch(2)).unwrap();
    uploader.stage_batch(batch(3)).unwrap();
    assert_eq!(next_attempt(&mut started).await.0, 1);
    assert!(uploader.is_refused());
    assert_eq!(uploader.pending_len(), 0);
    uploader.update_config(&TelemetryConfig {
        consent: Consent::Withdrawn,
        ..config.clone()
    });
    uploader.update_config(&config);
    assert!(matches!(
        uploader.queue_batch(batch(4)),
        Err(UploadError::Refused)
    ));
    assert!(started.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn withdrawal_during_backoff_invalidates_old_generation_even_after_regrant() {
    let (mock, mut started) = mock(vec![Reply::Retry, Reply::Sent]);
    let mut config = config();
    let mut recorder = SessionRecorder::new(session(), &config);
    let uploader = TelemetryUploader::new(&Handle::current(), &config, mock);
    uploader.queue_batch(batch(1)).unwrap();
    uploader.queue_batch(batch(2)).unwrap();
    recorder.record(&config, 3).unwrap();
    assert_eq!(next_attempt(&mut started).await.0, 1);
    recorder
        .withdraw_consent(&mut config, Some(&uploader), false)
        .unwrap();
    assert_eq!(uploader.pending_len(), 0);
    assert_eq!(recorder.pending_len(), 0);
    assert!(matches!(
        uploader.queue_batch(batch(4)),
        Err(UploadError::NotAllowed)
    ));
    config.consent = Consent::Granted;
    uploader.update_config(&config);
    uploader.queue_batch(batch(5)).unwrap();
    assert_eq!(next_attempt(&mut started).await.0, 5);
}

#[tokio::test]
async fn withdrawal_cancels_an_in_flight_transport_future() {
    let (mock, mut started) = mock(vec![Reply::Pending]);
    let cancelled = mock.cancellations.clone();
    let mut config = config();
    let mut recorder = SessionRecorder::new(session(), &config);
    let uploader = TelemetryUploader::new(&Handle::current(), &config, mock);
    uploader.queue_batch(batch(1)).unwrap();
    assert_eq!(next_attempt(&mut started).await.0, 1);
    recorder
        .withdraw_consent(&mut config, Some(&uploader), false)
        .unwrap();
    config.consent = Consent::Granted;
    uploader.update_config(&config);
    uploader.queue_batch(batch(2)).unwrap();
    assert_eq!(next_attempt(&mut started).await.0, 2);
    assert_eq!(cancelled.load(Ordering::Relaxed), 1);
}
