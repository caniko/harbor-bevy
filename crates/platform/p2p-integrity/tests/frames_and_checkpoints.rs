use p2p_integrity::{
    ChainError, CommitmentChain,
    compact_chain::{
        CompactChainError, from_state_commitment, to_state_commitment, verify_compact_checkpoints,
    },
    versioned_frame::{
        VersionedFrameError, build_magic_versioned_frame, build_versioned_frame,
        split_magic_versioned_frame, split_versioned_frame,
    },
};

#[test]
fn frames_validate_prefix_and_preserve_payload() {
    let body = b"\0body\xff";
    let plain = build_versioned_frame(0x1234, body);
    assert_eq!(plain, b"\x34\x12\0body\xff");
    assert_eq!(
        split_versioned_frame(&plain, &[1, 0x1234]),
        Ok((0x1234, body.as_slice()))
    );
    assert_eq!(
        split_versioned_frame(&plain, &[1]),
        Err(VersionedFrameError::UnsupportedVersion(0x1234))
    );
    assert_eq!(
        split_versioned_frame(&[1], &[1]),
        Err(VersionedFrameError::TruncatedVersion)
    );
    assert_eq!(split_versioned_frame(&[1, 0], &[1]), Ok((1, [].as_slice())));
    let magic = b"TEST";
    let frame = build_magic_versioned_frame(magic, 0x1234, body);
    assert_eq!(frame, b"TEST\x34\x12\0body\xff");
    assert_eq!(
        split_magic_versioned_frame(&frame, magic, &[0x1234]),
        Ok((0x1234, body.as_slice()))
    );
    assert_eq!(
        split_magic_versioned_frame(&frame, b"FAIL", &[0x1234]),
        Err(VersionedFrameError::InvalidMagic)
    );
    assert_eq!(
        split_magic_versioned_frame(b"TES", magic, &[0x1234]),
        Err(VersionedFrameError::InvalidMagic)
    );
    assert_eq!(
        split_magic_versioned_frame(magic, magic, &[0x1234]),
        Err(VersionedFrameError::TruncatedVersion)
    );
}

#[test]
fn compact_checkpoints_round_trip_and_reject_broken_chains() {
    let mut chain = CommitmentChain::new([1; 32]);
    chain.append([2; 32], [3; 32]);
    let first = from_state_commitment(&chain.commitments[0], 0);
    let second = from_state_commitment(&chain.commitments[1], 9);
    assert_eq!(
        to_state_commitment(&second, first.commitment_hash),
        chain.commitments[1]
    );
    assert_eq!(verify_compact_checkpoints(&[first, second]), Ok(()));
    assert_eq!(
        verify_compact_checkpoints(&[]),
        Err(ChainError::EmptyChain.into())
    );
    let mut bad = second;
    bad.sequence = 2;
    assert_eq!(
        verify_compact_checkpoints(&[first, bad]),
        Err(ChainError::SequenceGap {
            expected: 1,
            found: 2
        }
        .into())
    );
    bad = second;
    bad.state_hash[0] ^= 1;
    assert_eq!(
        verify_compact_checkpoints(&[first, bad]),
        Err(ChainError::InvalidCommitmentHash { sequence: 1 }.into())
    );
    bad = second;
    bad.tick = 0;
    assert_eq!(
        verify_compact_checkpoints(&[first, bad]),
        Err(CompactChainError::NonIncreasingTick { sequence: 1 })
    );
}
