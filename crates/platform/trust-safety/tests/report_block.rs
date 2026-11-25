use ed25519_dalek::SigningKey;
use trust_safety::{
    ActionGates, BlockList, BoundedEvidence, DenyRule, PeerIdentity, PresetMessageId, ReportReason,
    SafetyReport, generate_alias, sign_report, verify_report,
};

#[test]
fn block_denies_every_gate() {
    let mut list = BlockList::default();
    let blocked = PeerIdentity(b"peer-a".to_vec());
    let other = PeerIdentity(b"peer-b".to_vec());
    list.insert(blocked.clone());
    assert_eq!(
        list.gates(&blocked),
        ActionGates {
            matching: false,
            invites: false,
            reconnects: false,
            messages: false,
            tournament_co_placement: false,
        }
    );
    assert!(list.gates(&other).matching);
    assert!(list.gates(&other).invites);
}

#[test]
fn signed_report_round_trips_without_freeform() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let report = SafetyReport {
        reason: ReportReason::AbusiveConduct,
        evidence: BoundedEvidence {
            peer_id: b"target".to_vec(),
            match_id: Some([9; 16]),
            mode: Some(1),
            preset_message: Some(PresetMessageId::Hello),
            timestamp: 100,
        },
        reporter: PeerIdentity(b"reporter".to_vec()),
        target: PeerIdentity(b"target".to_vec()),
        issued_at: 200,
    };
    let mut signed = sign_report(report, &key);
    verify_report(&signed).unwrap();
    signed.report.evidence.mode = Some(0);
    assert!(verify_report(&signed).is_err());
}

#[test]
fn deny_rule_is_always_reviewed() {
    let rule = DenyRule::reviewed(1, PeerIdentity(b"x".to_vec()));
    assert!(rule.is_reviewed());
}

#[test]
fn remove_and_reviewed_overlay() {
    let mut list = BlockList::default();
    let id = PeerIdentity(b"peer-a".to_vec());
    list.insert(id.clone());
    assert!(list.remove(&id));
    assert!(list.gates(&id).matching);
    list.apply_reviewed([DenyRule::reviewed(1, id.clone())]);
    assert!(!list.gates(&id).matching);
}

#[test]
fn alias_comes_from_allowlist() {
    let mut rng = rand::rng();
    let alias = generate_alias(&mut rng);
    assert!(alias.as_str().chars().all(|c| c.is_ascii_alphabetic()));
    assert!(alias.as_str().len() >= 6);
}
