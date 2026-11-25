use attestation_broker::FileReportSink;

const DAY: u64 = 24 * 60 * 60;
const NOW: u64 = 1_700_000_000;

fn report(seed: u8, issued_at: u64) -> trust_safety::SignedReport {
    trust_safety::sign_report_with_seed(
        trust_safety::SafetyReport {
            reason: trust_safety::ReportReason::AbusiveConduct,
            evidence: trust_safety::BoundedEvidence {
                peer_id: vec![1],
                match_id: None,
                mode: None,
                preset_message: None,
                timestamp: NOW,
            },
            reporter: trust_safety::PeerIdentity(vec![2]),
            target: trust_safety::PeerIdentity(vec![1]),
            issued_at,
        },
        &[seed; 32],
    )
}

#[test]
fn retention_uses_receipt_and_reviewed_preservation() {
    let dir = std::env::temp_dir().join(format!("broker_retention_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("reports.json");
    let mut sink = FileReportSink::open(path.clone()).unwrap();
    sink.store_at("ordinary", &report(1, NOW), NOW).unwrap();
    sink.store_at("future", &report(2, NOW + 500 * DAY), NOW)
        .unwrap();
    sink.store_at("hold", &report(3, NOW), NOW).unwrap();
    sink.store_at("excessive", &report(4, NOW), NOW).unwrap();
    drop(sink);

    // Operator edits the local store; the HTTP report has no preservation field.
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    json[2]["preserved_until"] = (NOW + 190 * DAY).into();
    json[3]["preserved_until"] = (NOW + 361 * DAY).into();
    std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    let mut sink = FileReportSink::open(path.clone()).unwrap();
    assert_eq!(sink.prune_at(NOW + 179 * DAY).unwrap(), 0);
    assert_eq!(sink.prune_at(NOW + 180 * DAY).unwrap(), 3);
    let persisted = FileReportSink::open(path.clone()).unwrap();
    assert_eq!(persisted.reports().len(), 1);
    assert_eq!(persisted.reports()[0].subject, "hold");
    assert_eq!(sink.prune_at(NOW + 190 * DAY).unwrap(), 1);
    assert!(FileReportSink::open(path).unwrap().reports().is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}
