use std::{path::PathBuf, process::Command, time::Duration};

use bevy::prelude::{App, Camera2d};
use image::{Rgba, RgbaImage};
use tempfile::tempdir;

use crate::{
    FixtureSpec, Resolution, VisualSuite,
    audit::{AuditCheck, AuditInput, AuditNode, AuditRect, AuditSpec, NonBlankRegion},
    capture::{CaptureConfig, CaptureError, capture_after_frames, headless_app},
    coverage_store::{CoverageStore, ValidationKind, ValidationStatus},
    diff::{Mask, SnapshotError, SnapshotOptions, apply_masks, compare_snapshot},
    gpu_pin::{GpuAdapterIdentity, GpuPinError, ValidationDecision, validate_with_lookup},
    pipeline::{PipelineOptions, run_pipeline},
    report::{CellOutcome, CellRecord, GitInfo, RunConfig, RunReport},
    rubric::{Rubric, RubricBackend, RubricError, RubricVerdict},
    store_capture::{StoreCaptureLog, StoreCropSpec},
    worker_sidecar::{
        RenderedCellDescriptor, SIDECAR_VERSION, SidecarError, WORKER_SIDECAR_ENV, WorkerOutcome,
        WorkerResult, WorkerSidecar, read_sidecar, run_worker, write_sidecar_atomic,
    },
};

fn pixels(width: u32, height: u32, value: u8) -> RgbaImage {
    RgbaImage::from_pixel(width, height, Rgba([value, value, value, 255]))
}

#[test]
fn snapshot_mssim_and_updates() {
    let dir = tempdir().unwrap();
    let baseline = dir.path().join("baseline.png");
    let actual = dir.path().join("actual.webp");
    let compare = || compare_snapshot(&baseline, &actual, &[], SnapshotOptions::compare(0.99));
    pixels(10, 10, 250).save(&actual).unwrap();
    assert!(matches!(compare(), Err(SnapshotError::BaselineMissing(_))));
    compare_snapshot(&baseline, &actual, &[], SnapshotOptions::update()).unwrap();
    assert_eq!(compare().unwrap(), Some(1.0));
    pixels(9, 10, 250).save(&actual).unwrap();
    assert!(matches!(compare(), Err(SnapshotError::SizeMismatch { .. })));
    pixels(10, 10, 0).save(&actual).unwrap();
    let Err(SnapshotError::BelowThreshold { diff_path, .. }) = compare() else {
        panic!("expected a threshold failure");
    };
    assert_eq!(diff_path, dir.path().join("actual.diff.webp"));
    assert_eq!(image::open(diff_path).unwrap().width(), 10);
}

#[test]
fn masks_clip_and_compare_without_changing_inputs() {
    let mut image = pixels(4, 3, 255);
    let mask = Mask {
        x: 2,
        y: 1,
        w: u32::MAX,
        h: 2,
    };
    let original = image.clone();
    apply_masks(&mut image, &[mask]);
    assert_eq!(image.get_pixel(2, 1), &Rgba([0, 0, 0, 255]));
    assert_eq!(image.get_pixel(3, 2), &Rgba([0, 0, 0, 255]));
    assert_eq!(image.get_pixel(2, 0), original.get_pixel(2, 0));
    assert_eq!(image.get_pixel(1, 1), original.get_pixel(1, 1));
    let dir = tempdir().unwrap();
    let baseline = dir.path().join("baseline.png");
    let actual = dir.path().join("actual.png");
    original.save(&baseline).unwrap();
    image.save(&actual).unwrap();
    assert_eq!(
        compare_snapshot(&baseline, &actual, &[mask], SnapshotOptions::compare(1.0)).unwrap(),
        Some(1.0)
    );
    assert_eq!(image::open(baseline).unwrap().to_rgba8(), original);
}

#[test]
fn audits_aggregate_geometry_text_and_ink_failures() {
    let spec = AuditSpec {
        checks: vec![
            AuditCheck::Bounds,
            AuditCheck::NoOverlap {
                first: "title".into(),
                second: "status".into(),
            },
            AuditCheck::RequireText("Welcome".into()),
            AuditCheck::ForbidText("placeholder".into()),
            AuditCheck::NonBlank(NonBlankRegion {
                rect: Mask {
                    x: 0,
                    y: 0,
                    w: 10,
                    h: 10,
                },
                min_ink_fraction: 0.1,
            }),
        ],
    };
    let mut input = AuditInput {
        width: 10,
        height: 10,
        nodes: vec![
            AuditNode::text("title", "placeholder", AuditRect::new(0.0, 0.0, 6.0, 5.0)),
            AuditNode::text("status", "Ready", AuditRect::new(5.0, 1.0, 6.0, 4.0)),
        ],
    };
    let mut image = pixels(10, 10, 0);
    let report = spec.evaluate(&input, Some(&image)).unwrap();
    assert_eq!(
        serde_json::from_str::<AuditSpec>(&serde_json::to_string(&spec).unwrap()).unwrap(),
        spec
    );
    assert_eq!(
        serde_json::from_str::<crate::audit::AuditReport>(&serde_json::to_string(&report).unwrap())
            .unwrap(),
        report
    );
    assert_eq!(report.checked, 5);
    assert_eq!(report.failures.len(), 5);
    assert!(report.assert_pass().is_err());
    input.nodes[0].text = Some("Welcome".into());
    input.nodes[1].rect = AuditRect::new(6.0, 1.0, 4.0, 4.0);
    for x in 0..5 {
        for y in 0..10 {
            image.put_pixel(x, y, Rgba([255, 255, 255, 255]));
        }
    }
    assert!(
        spec.evaluate(&input, Some(&image))
            .unwrap()
            .assert_pass()
            .is_ok()
    );
    assert!(spec.evaluate(&input, None).is_err());
    input.nodes[1].rect.x = f32::NAN;
    assert!(spec.evaluate(&input, Some(&image)).is_err());
}

fn fixture() -> FixtureSpec {
    let mut fixture = FixtureSpec::new("settings", vec![Resolution::new("small", 40, 30)]);
    fixture.surface_ids.push("preferences".into());
    fixture.anchors.push("title".into());
    fixture
}

fn report_fixture() -> (FixtureSpec, RunReport) {
    let fixture = fixture();
    let mut report = RunReport::new("test-run", GitInfo::default(), RunConfig::default());
    let mut cell = CellRecord::new(&fixture, &fixture.viewports[0]);
    cell.actual_path = Some("settings/actual.png".into());
    cell.baseline_path = Some("settings/baseline.png".into());
    cell.outcome = CellOutcome::Pass {
        ssim: 1.0,
        rubric: None,
    };
    report.cells.push(cell);
    report.refresh_summary();
    (fixture, report)
}

#[test]
fn report_round_trips_and_rejects_duplicate_and_forged_results() {
    let (_, mut report) = report_fixture();
    let dir = tempdir().unwrap();
    let path = dir.path().join("report.json");
    report.write_atomic(&path).unwrap();
    assert_eq!(RunReport::read(&path).unwrap(), report);
    assert_eq!(report.summary.passed_cells, 1);
    assert!(report.validate_for_pass().is_ok());
    let mut failure = report.cells[0].clone();
    failure.viewport_label = "large".into();
    failure.outcome = CellOutcome::WorkerTimeout { elapsed_ms: 12 };
    report.cells.insert(0, failure.clone());
    report.refresh_summary();
    assert_eq!(report.summary.failed_cells, 1);
    assert_eq!(report.summary.scenarios_with_failures, vec!["settings"]);
    assert!(report.validate().is_ok());
    assert!(report.validate_for_pass().is_err());
    report.failures[0].width = 1;
    assert!(report.validate().is_err());
    report.cells.push(failure);
    report.refresh_summary();
    assert!(report.validate().is_err());
}

#[test]
fn coverage_store_persists_report_fixture_and_per_surface_results() {
    let (fixture, report) = report_fixture();
    let dir = tempdir().unwrap();
    let path = dir.path().join("coverage.db");
    let mut invalid = fixture.clone();
    invalid.viewports[0].height = 99;
    assert!(
        CoverageStore::open(&path)
            .unwrap()
            .record_report(&report, &[invalid])
            .is_err()
    );
    assert!(
        CoverageStore::open(&path)
            .unwrap()
            .load_report("test-run")
            .unwrap()
            .is_none()
    );
    CoverageStore::open(&path)
        .unwrap()
        .record_report(&report, std::slice::from_ref(&fixture))
        .unwrap();
    let mut store = CoverageStore::open(&path).unwrap();
    assert!(store.record_report(&report, &[fixture]).is_err());
    assert_eq!(store.load_report("test-run").unwrap(), Some(report));
    let rows = store.validations("test-run").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].surface_id, "preferences");
    assert_eq!(rows[0].kind, ValidationKind::Ssim);
    assert_eq!(rows[0].status, ValidationStatus::Passed);
    assert_eq!(store.anchors("settings").unwrap(), ["title"]);
}

const FAKE_ACTUAL_DIR: &str = "HARBOR_VISUAL_TEST_FAKE_ACTUAL_DIR";

struct PipelineSuite;

impl VisualSuite for PipelineSuite {
    fn fixtures(&self) -> Vec<FixtureSpec> {
        let mut fixture = fixture();
        fixture.viewports.push(Resolution::new("wide", 80, 40));
        fixture.rubric = Some(Rubric {
            name: "button clarity".into(),
            question: "Is the button readable?".into(),
            model: None,
            effort: None,
            system_prompt: None,
        });
        vec![fixture]
    }

    fn build(&self, _: &FixtureSpec, _: &mut App) {
        panic!("the worker protocol test must not initialize a renderer");
    }
}

#[test]
fn fake_worker_entry() {
    let Some(root) = std::env::var_os(FAKE_ACTUAL_DIR) else {
        return;
    };
    let fixture = PipelineSuite.fixtures().remove(0);
    let mut sidecar = WorkerSidecar::new(
        &fixture.name,
        fixture.viewports.iter().map(|v| v.label.clone()).collect(),
    );
    for viewport in fixture.viewports {
        let path = PathBuf::from(&root)
            .join(&fixture.name)
            .join(&viewport.label)
            .join("actual.png");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        pixels(viewport.width, viewport.height, 50)
            .save(&path)
            .unwrap();
        sidecar.cells.push(RenderedCellDescriptor {
            viewport,
            actual_path: path,
            audit: None,
        });
    }
    if std::env::var_os("HARBOR_VISUAL_TEST_INCOMPLETE_WORKER").is_some() {
        sidecar.cells.pop();
    }
    write_sidecar_atomic(
        &PathBuf::from(std::env::var_os(WORKER_SIDECAR_ENV).unwrap()),
        &sidecar,
    )
    .unwrap();
}

#[test]
fn pipeline_scores_workers_and_rejects_incomplete_success() {
    struct RateLimited;
    impl RubricBackend for RateLimited {
        fn evaluate(&self, _: &std::path::Path, _: &Rubric) -> Result<RubricVerdict, RubricError> {
            Err(RubricError::RateLimited {
                message: "try again".into(),
                retry_after_ms: Some(500),
            })
        }
    }
    let dir = tempdir().unwrap();
    let mut options = PipelineOptions::new(dir.path().join("baseline"), dir.path().join("actual"));
    let worker = |incomplete: bool| {
        move |_: &FixtureSpec, dir: &std::path::Path| {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", "tests::fake_worker_entry"])
                .env(FAKE_ACTUAL_DIR, dir);
            if incomplete {
                command.env("HARBOR_VISUAL_TEST_INCOMPLETE_WORKER", "1");
            }
            command
        }
    };
    options.update_snapshots = true;
    let report = run_pipeline(
        &PipelineSuite,
        "update",
        GitInfo::default(),
        &options,
        None,
        worker(false),
    )
    .unwrap();
    assert_eq!(report.summary.baselines_updated, 2);
    assert!(report.validate_for_pass().is_err());

    options.update_snapshots = false;
    let report = run_pipeline(
        &PipelineSuite,
        "check",
        GitInfo::default(),
        &options,
        None,
        worker(false),
    )
    .unwrap();
    assert_eq!(report.summary.ssim_pass, 2);
    assert!(report.validate_for_pass().is_ok());

    let report = run_pipeline(
        &PipelineSuite,
        "rubric",
        GitInfo::default(),
        &options,
        Some(&RateLimited),
        worker(false),
    )
    .unwrap();
    assert_eq!(report.summary.rubric_fail, 2);
    assert_eq!(report.rate_limit_events[0].retry_after_ms, Some(500));

    let report = run_pipeline(
        &PipelineSuite,
        "incomplete",
        GitInfo::default(),
        &options,
        None,
        worker(true),
    )
    .unwrap();
    assert_eq!(report.summary.worker_crashed, 2);
}

#[test]
fn sidecar_atomically_replaces_and_rejects_unknown_versions() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("status/worker.json");
    let mut sidecar = WorkerSidecar::new("settings", vec!["small".into()]);
    write_sidecar_atomic(&path, &sidecar).unwrap();
    sidecar.outcome = WorkerOutcome::SetupError {
        message: "setup failed".into(),
    };
    write_sidecar_atomic(&path, &sidecar).unwrap();
    assert_eq!(read_sidecar(&path).unwrap(), sidecar);
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
        1
    );
    std::fs::write(&path, br#"{"version":999,"unknown_future_shape":true}"#).unwrap();
    assert!(matches!(
        read_sidecar(&path),
        Err(SidecarError::UnsupportedVersion {
            found: 999,
            expected: SIDECAR_VERSION
        })
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn worker_classifies_setup_timeout_crash_and_partial_panic() {
    let dir = tempdir().unwrap();
    let setup = run_worker(
        Command::new(dir.path().join("absent")),
        Duration::from_secs(1),
    );
    assert!(matches!(setup, WorkerResult::SetupError { .. }));
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "sleep 10 & wait"]);
    assert!(matches!(
        run_worker(command, Duration::from_millis(30)),
        WorkerResult::Timeout { .. }
    ));
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "exit 5"]);
    assert!(matches!(
        run_worker(command, Duration::from_secs(1)),
        WorkerResult::Crash { .. }
    ));
    let mut command = Command::new("/bin/sh");
    command.args([
        "-c",
        r#"echo '{"version":1,"fixture":"settings","viewports_requested":["small"],"outcome":{"kind":"partial_panic","message":"panic","panicked_viewport":"small"},"cells":[]}' > "$HARBOR_VISUAL_WORKER_SIDECAR""#,
    ]);
    assert!(matches!(
        run_worker(command, Duration::from_secs(1)),
        WorkerResult::PartialPanic(_)
    ));
}

#[test]
fn store_crop_and_log_round_trip() {
    let mut image = pixels(4, 3, 9);
    image.put_pixel(2, 1, Rgba([100, 0, 0, 255]));
    let mut crop = StoreCropSpec {
        output: "cropped.webp".into(),
        region: Mask {
            x: 2,
            y: 1,
            w: 2,
            h: 2,
        },
        output_size: None,
    };
    let result = crop.apply(&image).unwrap();
    assert_eq!(result.get_pixel(0, 0), image.get_pixel(2, 1));
    crop.output_size = Some([4, 4]);
    assert_eq!(crop.apply(&image).unwrap().dimensions(), (4, 4));
    crop.region.w = u32::MAX;
    assert!(crop.apply(&image).is_err());
    let dir = tempdir().unwrap();
    let mut log = StoreCaptureLog::default();
    log.artifacts.push(crate::store_capture::StoreArtifact {
        fixture: "settings".into(),
        viewport: "small".into(),
        source: Some("actual.png".into()),
        output: crop.output,
        width: 4,
        height: 4,
    });
    log.write(dir.path()).unwrap();
    assert_eq!(StoreCaptureLog::read(dir.path()).unwrap(), log);
}

#[test]
fn gpu_pin_parses_ids_and_matches_normalized_adapter_fields() {
    let gpu = GpuAdapterIdentity {
        backend: "Vulkan".into(),
        vendor: 0x1002,
        device: 42,
        device_type: "DiscreteGpu".into(),
        driver_info: "Mesa 23".into(),
        ..GpuAdapterIdentity::default()
    };
    let validate = |pairs: &[(&str, &str)]| {
        validate_with_lookup(&gpu, |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        })
    };
    assert!(matches!(validate(&[]), Err(GpuPinError::MissingPin { .. })));
    assert!(matches!(
        validate(&[("RS_HARBOR_GPU_EXPECTED_VENDOR", "0x100000000")]),
        Err(GpuPinError::InvalidExpected { .. })
    ));
    assert!(matches!(
        validate(&[("RS_HARBOR_GPU_EXPECTED_DEVICE", "9")]),
        Err(GpuPinError::Mismatch { .. })
    ));
    assert_eq!(
        validate(&[
            ("RS_HARBOR_GPU_PIN_PROFILE", "local"),
            ("RS_HARBOR_GPU_EXPECTED_BACKEND", " VULKAN "),
            ("RS_HARBOR_GPU_EXPECTED_VENDOR", "0X1002"),
            ("RS_HARBOR_GPU_EXPECTED_DEVICE", "42"),
            ("RS_HARBOR_GPU_EXPECTED_DRIVER_INFO_CONTAINS", "mesa"),
        ])
        .unwrap(),
        ValidationDecision::Pinned
    );
    assert_eq!(
        validate(&[("HARBOR_VISUAL_TEST_ALLOW_UNPINNED", "1")]).unwrap(),
        ValidationDecision::AllowedUnpinned
    );
}

#[test]
fn offscreen_api_compiles_and_rejects_invalid_config_without_a_gpu() {
    struct Example;
    impl VisualSuite for Example {
        fn fixtures(&self) -> Vec<FixtureSpec> {
            vec![fixture()]
        }
        fn build(&self, _: &FixtureSpec, app: &mut App) {
            app.world_mut().spawn(Camera2d);
        }
    }
    let fixture = Example.fixtures().remove(0);
    let mut app = App::new();
    Example.build(&fixture, &mut app);
    assert!(Example.masks(&fixture).is_empty());
    let _headless_constructor: fn() -> App = headless_app;
    let mut config = CaptureConfig::new(PathBuf::from("actual.png"));
    config.width = 0;
    assert!(matches!(
        capture_after_frames(&mut app, &config),
        Err(CaptureError::InvalidConfig(_))
    ));
}
