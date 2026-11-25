#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
    struct TestAction {
        from: String,
        to: String,
    }

    impl SignableAction for TestAction {
        fn signing_bytes(&self) -> Vec<u8> {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(self.from.as_bytes());
            bytes.push(0); // separator
            bytes.extend_from_slice(self.to.as_bytes());
            bytes
        }
    }

    #[test]
    fn test_key_pair_generation() {
        let key1 = PlayerKeyPair::generate();
        let key2 = PlayerKeyPair::generate();

        // Keys should be different
        assert_ne!(key1.public_key, key2.public_key);

        // Public key should be 32 bytes
        assert_eq!(key1.public_key.len(), 32);
    }

    #[test]
    fn test_key_pair_from_seed() {
        let seed = [42u8; 32];
        let key1 = PlayerKeyPair::from_seed(&seed);
        let key2 = PlayerKeyPair::from_seed(&seed);

        // Same seed should produce same key
        assert_eq!(key1.public_key, key2.public_key);
    }

    #[test]
    fn test_sign_and_verify() {
        let key_pair = PlayerKeyPair::generate();

        let action = TestAction {
            from: "e2".to_string(),
            to: "e4".to_string(),
        };

        let metadata = ActionMetadata {
            sequence: 0,
            timestamp_ms: 1000,
            match_id: [0u8; 16],
            player_index: 0,
        };

        let prev_hash = [0u8; 32];

        let signed = SignedAction::sign(action.clone(), metadata, prev_hash, &key_pair);

        // Signature should be valid
        assert!(signed.verify());

        // Action should be preserved
        assert_eq!(signed.action, action);

        // Public key should match
        assert_eq!(signed.public_key, key_pair.public_key);
    }

    #[test]
    fn test_verify_fails_with_wrong_key() {
        let key_pair1 = PlayerKeyPair::generate();
        let key_pair2 = PlayerKeyPair::generate();

        let action = TestAction {
            from: "e2".to_string(),
            to: "e4".to_string(),
        };

        let metadata = ActionMetadata {
            sequence: 0,
            timestamp_ms: 1000,
            match_id: [0u8; 16],
            player_index: 0,
        };

        let signed = SignedAction::sign(action, metadata, [0u8; 32], &key_pair1);

        // Create a tampered version with different public key
        let tampered = SignedAction {
            public_key: key_pair2.public_key,
            ..signed
        };

        // Verification should fail
        assert!(!tampered.verify());
    }

    #[test]
    fn test_verify_fails_with_tampered_action() {
        let key_pair = PlayerKeyPair::generate();

        let action = TestAction {
            from: "e2".to_string(),
            to: "e4".to_string(),
        };

        let metadata = ActionMetadata {
            sequence: 0,
            timestamp_ms: 1000,
            match_id: [0u8; 16],
            player_index: 0,
        };

        let signed = SignedAction::sign(action, metadata, [0u8; 32], &key_pair);

        // Tamper with the action
        let tampered = SignedAction {
            action: TestAction {
                from: "e2".to_string(),
                to: "e5".to_string(), // Changed!
            },
            ..signed
        };

        // Verification should fail
        assert!(!tampered.verify());
    }

    #[test]
    fn test_verify_fails_with_tampered_prev_hash() {
        let key_pair = PlayerKeyPair::generate();

        let action = TestAction {
            from: "e2".to_string(),
            to: "e4".to_string(),
        };

        let metadata = ActionMetadata {
            sequence: 0,
            timestamp_ms: 1000,
            match_id: [0u8; 16],
            player_index: 0,
        };

        let signed = SignedAction::sign(action, metadata, [0u8; 32], &key_pair);

        // Tamper with prev_hash
        let tampered = SignedAction {
            prev_hash: [1u8; 32], // Changed!
            ..signed
        };

        // Verification should fail
        assert!(!tampered.verify());
    }

    #[test]
    fn test_compute_hash_is_deterministic() {
        let key_pair = PlayerKeyPair::generate();

        let action = TestAction {
            from: "e2".to_string(),
            to: "e4".to_string(),
        };

        let metadata = ActionMetadata {
            sequence: 0,
            timestamp_ms: 1000,
            match_id: [0u8; 16],
            player_index: 0,
        };

        let signed = SignedAction::sign(action, metadata, [0u8; 32], &key_pair);

        let hash1 = signed.compute_hash();
        let hash2 = signed.compute_hash();

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_compute_hash_differs_for_different_actions() {
        let key_pair = PlayerKeyPair::generate();
        let match_id = [42u8; 16];

        let action1 = TestAction {
            from: "e2".to_string(),
            to: "e4".to_string(),
        };

        let action2 = TestAction {
            from: "d2".to_string(),
            to: "d4".to_string(),
        };

        let metadata1 = ActionMetadata {
            sequence: 0,
            timestamp_ms: 1000,
            match_id,
            player_index: 0,
        };

        let metadata2 = ActionMetadata {
            sequence: 1,
            timestamp_ms: 2000,
            match_id,
            player_index: 1,
        };

        let signed1 = SignedAction::sign(action1, metadata1, [0u8; 32], &key_pair);
        let signed2 = SignedAction::sign(action2, metadata2, [0u8; 32], &key_pair);

        assert_ne!(signed1.compute_hash(), signed2.compute_hash());
    }

    #[test]
    fn test_verify_signature_standalone() {
        let key_pair = PlayerKeyPair::generate();
        let message = b"Hello, World!";
        let signature = key_pair.sign(message);

        assert!(verify_signature(message, &signature, &key_pair.public_key));
        assert!(!verify_signature(
            b"Wrong message",
            &signature,
            &key_pair.public_key
        ));
    }

    #[test]
    fn malformed_signature_inputs_return_false_without_panicking() {
        let key_pair = PlayerKeyPair::from_seed(&[5; 32]);
        let action = TestAction {
            from: "a2".to_string(),
            to: "a3".to_string(),
        };
        let metadata = ActionMetadata {
            sequence: 1,
            timestamp_ms: 10,
            match_id: [1u8; 16],
            player_index: 0,
        };
        let signed = SignedAction::sign(action, metadata, [0u8; 32], &key_pair);

        let bad_public_key = SignedAction {
            public_key: [0xff; 32],
            ..signed.clone()
        };
        let unsigned = SignedAction {
            signature: [0; 64],
            public_key: [0; 32],
            ..signed
        };

        assert!(!bad_public_key.verify());
        assert!(!unsigned.verify());
        assert!(unsigned.is_unsigned());
        assert!(!verify_signature(
            b"message",
            &[0u8; 64],
            &bad_public_key.public_key
        ));
    }

    #[test]
    fn test_serialization_roundtrip() {
        let key_pair = PlayerKeyPair::generate();

        let action = TestAction {
            from: "e2".to_string(),
            to: "e4".to_string(),
        };

        let metadata = ActionMetadata {
            sequence: 42,
            timestamp_ms: 12345,
            match_id: [0u8; 16],
            player_index: 1,
        };

        let signed = SignedAction::sign(action.clone(), metadata, [7u8; 32], &key_pair);

        // rkyv roundtrip
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&signed).unwrap();
        let restored: SignedAction<TestAction> =
            rkyv::from_bytes::<SignedAction<TestAction>, rkyv::rancor::Error>(&bytes).unwrap();

        assert_eq!(restored.action, action);
        assert_eq!(restored.metadata.sequence, 42);
        assert_eq!(restored.prev_hash, [7u8; 32]);
        assert!(restored.verify());
    }

    #[test]
    fn test_action_metadata_serialization() {
        let metadata = ActionMetadata {
            sequence: 100,
            timestamp_ms: 999999,
            match_id: [0u8; 16],
            player_index: 0,
        };

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&metadata).unwrap();
        let restored: ActionMetadata =
            rkyv::from_bytes::<ActionMetadata, rkyv::rancor::Error>(&bytes).unwrap();

        assert_eq!(restored.sequence, 100);
        assert_eq!(restored.timestamp_ms, 999999);
        assert_eq!(restored.match_id, metadata.match_id);
        assert_eq!(restored.player_index, 0);
    }
}
