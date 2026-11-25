//! Confidence levels for recording validation.

/// Confidence level based on witness attestations.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub enum ConfidenceLevel {
    None,
    Low,
    Medium,
    High,
    Consensus,
}

impl ConfidenceLevel {
    pub fn meets(self, required: Self) -> bool {
        self >= required
    }
}

impl std::fmt::Display for ConfidenceLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_levels_are_ordered() {
        assert!(ConfidenceLevel::High.meets(ConfidenceLevel::Medium));
        assert!(!ConfidenceLevel::Low.meets(ConfidenceLevel::High));
    }
}
