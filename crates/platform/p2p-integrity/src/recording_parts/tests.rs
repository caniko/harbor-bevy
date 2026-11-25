#[cfg(test)]
mod tests {
    use super::*;
    use crate::attestation::{ActionMetadata, PlayerKeyPair};

    #[derive(Debug, Clone)]
    struct TestAction(u32);

    impl SignableAction for TestAction {
        fn signing_bytes(&self) -> Vec<u8> {
            self.0.to_le_bytes().to_vec()
        }
    }

    #[test]
    fn recording_validates_signed_actions() {
        let key_pair = PlayerKeyPair::generate();
        let match_id = [42; 16];
        let mut recording = AttestedRecording::new(match_id, [0; 32]);
        let action = SignedAction::sign(
            TestAction(1),
            ActionMetadata {
                sequence: 0,
                timestamp_ms: 1000,
                match_id,
                player_index: 0,
            },
            [0; 32],
            &key_pair,
        );

        recording.add_action(action, [1; 32]);

        assert!(recording.validate().is_valid());
    }
}
