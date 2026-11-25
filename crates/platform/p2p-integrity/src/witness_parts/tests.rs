#[cfg(test)]
mod tests {
    use super::*;
    use crate::attestation::PlayerKeyPair;

    #[test]
    fn witness_attestation_verifies_its_signature() {
        let key_pair = PlayerKeyPair::generate();
        let attestation = WitnessAttestation::create(
            [1; 16],
            5,
            [42; 32],
            [2; 16],
            key_pair.public_key,
            WitnessVerdict::Valid,
            |message| key_pair.sign(message),
        );

        assert!(attestation.verify());
    }
}
