//! Reproducible PCG-XSH-RR streams. Not suitable for cryptographic use.
//!
//! With `rkyv`, checked archive access validates the layout; deserializing
//! into [`DeterministicRng`] additionally validates the stream increment.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(feature = "serde")]
mod serde_impl;

const PCG_MULTIPLIER: u64 = 6_364_136_223_846_793_005;

/// A deterministic stream whose snapshot consists of a state and an odd increment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "rkyv", derive(rkyv::Archive, rkyv::Serialize))]
pub struct DeterministicRng {
    state: u64,
    inc: u64,
}

impl DeterministicRng {
    /// Seed a stream with a UTF-8 domain and the seed's eight little-endian bytes.
    ///
    /// Panics if the domain exceeds `u32::MAX` bytes.
    pub fn from_seed(domain: &str, seed: u64) -> Self {
        Self::from_seed_bytes(domain.as_bytes(), &seed.to_le_bytes())
    }

    /// Seed a stream with arbitrary bytes using the frozen BLAKE3 framing:
    /// each of `domain` and `seed`, in that order, is prefixed by its LE `u32` length.
    ///
    /// Panics if either input exceeds `u32::MAX` bytes.
    pub fn from_seed_bytes(domain: &[u8], seed: &[u8]) -> Self {
        let mut hasher = blake3::Hasher::new();
        for bytes in [domain, seed] {
            let len = u32::try_from(bytes.len()).expect("PCG seed input length exceeds u32");
            hasher.update(&len.to_le_bytes());
            hasher.update(bytes);
        }
        let digest = hasher.finalize();
        let (state, rest) = digest.as_bytes().split_first_chunk::<8>().unwrap();
        let (sequence, _) = rest.split_first_chunk::<8>().unwrap();
        let inc = (u64::from_le_bytes(*sequence) << 1) | 1;
        let mut rng = Self { state: 0, inc };
        rng.next_u32();
        rng.state = rng.state.wrapping_add(u64::from_le_bytes(*state));
        rng.next_u32();
        rng
    }

    /// Snapshot the stream as `[state, increment]`.
    pub const fn state_words(&self) -> [u64; 2] {
        [self.state, self.inc]
    }

    /// Restore `[state, increment]`, rejecting even increments.
    pub const fn from_state_words([state, inc]: [u64; 2]) -> Option<Self> {
        if inc & 1 == 1 {
            Some(Self { state, inc })
        } else {
            None
        }
    }

    /// Draw the next PCG-XSH-RR word.
    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old.wrapping_mul(PCG_MULTIPLIER).wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        xorshifted.rotate_right((old >> 59) as u32)
    }

    /// Draw two words, placing the first draw in the low 32 bits.
    pub fn next_u64(&mut self) -> u64 {
        let low = u64::from(self.next_u32());
        low | (u64::from(self.next_u32()) << 32)
    }

    /// Draw an unbiased integer in `0..bound`. Panics if `bound` is zero.
    pub fn gen_range_u32(&mut self, bound: u32) -> u32 {
        self.try_gen_range_u32(bound)
            .expect("PCG range bound must be nonzero")
    }

    /// Draw an unbiased integer in `0..bound`, or return `None` for zero without advancing.
    pub fn try_gen_range_u32(&mut self, bound: u32) -> Option<u32> {
        if bound == 0 {
            return None;
        }
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let word = self.next_u32();
            if word >= threshold {
                return Some(word % bound);
            }
        }
    }
}

#[cfg(feature = "rkyv")]
impl<D> rkyv::Deserialize<DeterministicRng, D> for ArchivedDeterministicRng
where
    D: rkyv::rancor::Fallible + ?Sized,
    D::Error: rkyv::rancor::Source,
{
    fn deserialize(&self, _: &mut D) -> Result<DeterministicRng, D::Error> {
        let Some(rng) =
            DeterministicRng::from_state_words([self.state.to_native(), self.inc.to_native()])
        else {
            rkyv::rancor::fail!(InvalidIncrement);
        };
        Ok(rng)
    }
}

#[cfg(feature = "rkyv")]
#[derive(Debug)]
struct InvalidIncrement;

#[cfg(feature = "rkyv")]
impl core::fmt::Display for InvalidIncrement {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("PCG increment must be odd")
    }
}

#[cfg(feature = "rkyv")]
impl core::error::Error for InvalidIncrement {}
