//! Store verification data, item pool validation, and single-use ad permits.
#![no_std]

extern crate alloc;

use alloc::{collections::BTreeSet, string::String};
use core::fmt;

/// A single-use authorization to show an ad at placement `P`.
///
/// ```compile_fail,E0599
/// fn clone(permit: monetization_policy::AdPermit<u8>) {
///     let _ = permit.clone();
/// }
/// ```
///
/// ```compile_fail,E0382
/// fn reuse(permit: monetization_policy::AdPermit<u8>) {
///     let _ = permit.placement();
///     let _ = permit.placement();
/// }
/// ```
#[derive(Debug, PartialEq, Eq)]
pub struct AdPermit<P> {
    placement: P,
}

impl<P> AdPermit<P> {
    /// Consumes the permit and returns its authorized placement.
    #[must_use]
    pub fn placement(self) -> P {
        self.placement
    }
}

/// Runs the caller's policy for the exact placement before issuing a permit.
/// Issue and consume at the display boundary; this is not a server credential.
pub fn try_issue_ad_permit<P, E>(
    placement: P,
    authorize: impl FnOnce(&P) -> Result<(), E>,
) -> Result<AdPermit<P>, E> {
    authorize(&placement)?;
    Ok(AdPermit { placement })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub enum IapPlatform {
    AppleAppStore,
    GooglePlay,
}

/// Unverified store proof. Do not log or expose its contents.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub enum IapProof {
    AppleStoreKit2Jws {
        jws_transaction: String,
        is_restore: bool,
    },
    GooglePlayToken {
        product_id: String,
        purchase_token: String,
        order_id: Option<String>,
        is_restore: bool,
    },
}

impl IapProof {
    #[must_use]
    pub const fn platform(&self) -> IapPlatform {
        match self {
            Self::AppleStoreKit2Jws { .. } => IapPlatform::AppleAppStore,
            Self::GooglePlayToken { .. } => IapPlatform::GooglePlay,
        }
    }
}

impl fmt::Debug for IapProof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IapProof")
            .field("platform", &self.platform())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub enum IapClaimOutcome {
    Claimed,
    AlreadyClaimedBySameAccount,
    ReplayConflict,
}

/// Verification result parameterized over the application's offer type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub enum VerifyIapOutcome<O> {
    Granted(O),
    AlreadyGranted(O),
    RejectedInvalidSignature,
    RejectedWrongBundleOrEnv,
    RejectedAccountMismatch,
    RejectedUnknownOrMismatchedSku,
    RejectedRevokedOrUnpaid,
    RejectedReplayConflict,
    TransientStoreUnavailable,
}

/// An item ID shared by both pools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemPoolOverlap<T>(pub T);

impl<T> fmt::Display for ItemPoolOverlap<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("item pools share an item")
    }
}

impl<T: fmt::Debug> core::error::Error for ItemPoolOverlap<T> {}

/// Rejects the first item in `earned` that also occurs in `paid`.
/// Inputs should be resolved item IDs, not offer or SKU IDs.
pub fn validate_disjoint_item_pools<T: Ord>(
    paid: impl IntoIterator<Item = T>,
    earned: impl IntoIterator<Item = T>,
) -> Result<(), ItemPoolOverlap<T>> {
    let paid: BTreeSet<_> = paid.into_iter().collect();
    for item in earned {
        if paid.contains(&item) {
            return Err(ItemPoolOverlap(item));
        }
    }
    Ok(())
}
