use bevy::prelude::Resource;
use rand::{Rng as _, SeedableRng as _, rngs::StdRng};

pub const MAX_SNAPSHOT_WORDS: u64 = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum SharedRngSeed {
    Session([u8; 32]),
    Tournament(u64),
}

/// Position in the stream, in calls to `next_u64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct SharedRngState {
    pub seed: SharedRngSeed,
    pub words: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("RNG position {0} exceeds the snapshot limit")]
pub struct RngPositionError(pub u64);

/// Replicated peers must use the same build: `StdRng` does not guarantee a
/// stable generator across dependency updates.
#[derive(Resource)]
pub struct SharedRng {
    rng: StdRng,
    state: SharedRngState,
}

impl SharedRng {
    pub fn new(seed: SharedRngSeed) -> Self {
        let rng = match seed {
            SharedRngSeed::Session(bytes) => StdRng::from_seed(bytes),
            SharedRngSeed::Tournament(seed) => StdRng::seed_from_u64(seed),
        };
        Self {
            rng,
            state: SharedRngState { seed, words: 0 },
        }
    }

    pub fn from_negotiated_nonces(a: [u8; 32], b: [u8; 32]) -> Self {
        Self::new(SharedRngSeed::Session(nonce_seed(a, b)))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state.words = self.state.words.saturating_add(1);
        self.rng.next_u64()
    }

    pub fn snapshot_state(&self) -> Result<SharedRngState, RngPositionError> {
        validate_position(self.state.words)?;
        Ok(self.state)
    }

    pub fn restore_from_state(state: SharedRngState) -> Result<Self, RngPositionError> {
        validate_position(state.words)?;
        let mut rng = Self::new(state.seed);
        // ponytail: bounded linear seek; use a seekable RNG if snapshots need larger offsets.
        for _ in 0..state.words {
            rng.next_u64();
        }
        Ok(rng)
    }
}

pub(crate) fn nonce_seed(a: [u8; 32], b: [u8; 32]) -> [u8; 32] {
    std::array::from_fn(|index| a[index] ^ b[index])
}

fn validate_position(words: u64) -> Result<(), RngPositionError> {
    if words > MAX_SNAPSHOT_WORDS {
        Err(RngPositionError(words))
    } else {
        Ok(())
    }
}
