use harbor_deterministic_rng::DeterministicRng;

#[test]
fn numeric_seed_golden_vector() {
    let mut rng = DeterministicRng::from_seed("fixture", 42);
    assert_eq!(
        rng.state_words(),
        [0x9c43_0cf7_2c03_7713, 0xab6b_a432_902f_dee1]
    );
    assert_eq!(rng.next_u64(), 0xf423_d570_af9f_b10c);
    assert_eq!(rng.next_u32(), 0xf50b_6016);
    assert_eq!(rng.next_u64(), 0xe5d4_2a34_8635_bd32);
}

#[test]
fn byte_seed_golden_vector() {
    let mut rng = DeterministicRng::from_seed_bytes(b"\xff\0", b"\0abc");
    assert_eq!(
        rng.state_words(),
        [0x00a0_8277_14b7_e85d, 0xc48b_3840_a276_6f8b]
    );
    assert_eq!(
        core::array::from_fn::<_, 5, _>(|_| rng.next_u32()),
        [
            0x1410_4be6,
            0xe715_29af,
            0x26cb_d31c,
            0x446e_cb31,
            0x7308_201a,
        ]
    );
    assert_ne!(
        DeterministicRng::from_seed_bytes(b"a", b"bc"),
        DeterministicRng::from_seed_bytes(b"ab", b"c")
    );
}

#[test]
fn restored_state_rejects_even_increments() {
    assert_eq!(DeterministicRng::from_state_words([1, 2]), None);
    let mut rng = DeterministicRng::from_seed("fixture", 42);
    rng.next_u32();
    let mut restored = DeterministicRng::from_state_words(rng.state_words()).unwrap();
    assert_eq!(rng.next_u64(), restored.next_u64());
}

#[test]
fn range_rejects_the_biased_prefix() {
    let mut rng = DeterministicRng::from_seed_bytes(b"\xff\0", b"\0abc");
    assert_eq!(rng.try_gen_range_u32(0), None);
    assert_eq!(rng.gen_range_u32(0x8000_0001), 0x6715_29ae);
    assert_eq!(rng.next_u32(), 0x26cb_d31c);
    assert_eq!(rng.gen_range_u32(1), 0);
}

#[test]
#[should_panic(expected = "PCG range bound must be nonzero")]
fn zero_bound_panics() {
    DeterministicRng::from_seed("fixture", 42).gen_range_u32(0);
}

#[cfg(feature = "serde")]
#[test]
fn serde_preserves_named_fields_and_validates_increment() {
    let rng = DeterministicRng::from_state_words([7, 3]).unwrap();
    assert_eq!(
        serde_json::to_string(&rng).unwrap(),
        r#"{"state":7,"inc":3}"#
    );
    assert_eq!(
        serde_json::from_str::<DeterministicRng>(r#"{"inc":3,"state":7}"#).unwrap(),
        rng
    );
    for invalid in [
        r#"{"state":7,"inc":2}"#,
        "[7,2]",
        r#"{"state":7,"inc":1,"inc":3}"#,
        r#"{"inc":3}"#,
    ] {
        assert!(serde_json::from_str::<DeterministicRng>(invalid).is_err());
    }
}

#[cfg(feature = "rkyv")]
#[test]
fn rkyv_round_trip_rejects_even_archived_increment() {
    use rkyv::{
        api::low,
        rancor::Error,
        ser::{allocator::SubAllocator, writer::Buffer},
        util::Align,
    };

    let rng = DeterministicRng::from_state_words([7, 3]).unwrap();
    let mut storage = Align([0_u8; 16]);
    let bytes = low::to_bytes_in_with_alloc::<_, _, Error>(
        &rng,
        Buffer::from(&mut storage.0),
        SubAllocator::empty(),
    )
    .unwrap();
    assert_eq!(
        low::from_bytes::<DeterministicRng, Error>(&bytes).unwrap(),
        rng
    );
    storage.0[8..].copy_from_slice(&2u64.to_le_bytes());
    assert!(low::from_bytes::<DeterministicRng, Error>(&storage.0).is_err());
}
