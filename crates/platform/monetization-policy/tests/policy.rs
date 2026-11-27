use monetization_policy::{ItemPoolOverlap, try_issue_ad_permit, validate_disjoint_item_pools};

#[test]
fn issuance_requires_authorization_of_the_exact_placement() {
    let authorize = |placement: &&str| {
        if *placement == "reward" {
            Ok(())
        } else {
            Err("denied")
        }
    };
    assert_eq!(try_issue_ad_permit("banner", authorize), Err("denied"));
    assert_eq!(
        try_issue_ad_permit("reward", authorize)
            .unwrap()
            .placement(),
        "reward"
    );
}

#[test]
fn only_cross_pool_overlap_is_rejected() {
    assert_eq!(validate_disjoint_item_pools([1, 1], [2, 2]), Ok(()));
    assert_eq!(validate_disjoint_item_pools([], [2]), Ok(()));
    assert_eq!(
        validate_disjoint_item_pools([1, 2], [3, 2, 1]),
        Err(ItemPoolOverlap(2))
    );
}

#[test]
fn apple_proof_reports_its_store_without_logging_payload() {
    use monetization_policy::{IapPlatform, IapProof};
    let proof = IapProof::AppleStoreKit2Jws {
        jws_transaction: "not-for-logs".into(),
        is_restore: false,
    };
    assert_eq!(proof.platform(), IapPlatform::AppleAppStore);
    assert!(!format!("{proof:?}").contains("not-for-logs"));
}

#[cfg(any(feature = "serde", feature = "rkyv"))]
fn purchase() -> (
    monetization_policy::IapProof,
    monetization_policy::VerifyIapOutcome<u8>,
) {
    use monetization_policy::{IapProof, VerifyIapOutcome};
    (
        IapProof::GooglePlayToken {
            product_id: "sample.sku".into(),
            purchase_token: "opaque".into(),
            order_id: Some("order".into()),
            is_restore: true,
        },
        VerifyIapOutcome::AlreadyGranted(1),
    )
}

#[cfg(feature = "serde")]
#[test]
fn serde_round_trips_proof_and_generic_outcome() {
    use monetization_policy::{IapProof, VerifyIapOutcome};
    let original = purchase();
    let wire = serde_json::to_string(&original).unwrap();
    assert_eq!(
        serde_json::from_str::<(IapProof, VerifyIapOutcome<u8>)>(&wire).unwrap(),
        original
    );
}

#[cfg(feature = "rkyv")]
#[test]
fn rkyv_round_trips_proof_and_generic_outcome() {
    use monetization_policy::{IapProof, VerifyIapOutcome};
    use rkyv::rancor::Error;
    let original = purchase();
    let bytes = rkyv::to_bytes::<Error>(&original).unwrap();
    assert_eq!(
        rkyv::from_bytes::<(IapProof, VerifyIapOutcome<u8>), Error>(&bytes).unwrap(),
        original
    );
}
