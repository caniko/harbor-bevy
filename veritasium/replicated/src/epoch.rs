//! Session epoch: a monotonic `(match_id, battle_number, round_number)`
//! stamp for wire messages, plus the acceptance policy that drops stale
//! cross-battle traffic centrally instead of per-receiver.
//!
//! The game layer owns the mapping from its message kinds to an
//! [`EpochPolicy`] and the systems that advance [`LocalEpoch`] at battle
//! boundaries; this module owns the types and the acceptance comparison.

use bevy::prelude::*;
use veritasium_wire::{BattleNumber, RoundNumber};

/// Stamp identifying which match, which battle, and which round a wire
/// message belongs to. Comparable lexicographically:
/// `(match_id, battle_number, round_number)`.
///
/// `match_id` is a 16-byte deterministic match identifier (e.g. a hash of
/// the two peers' negotiation nonces). `battle_number` monotonically
/// increases across battles within a match. `round_number` increases
/// within a battle but resets at battle boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct SessionEpoch {
    pub match_id: [u8; 16],
    pub battle_number: BattleNumber,
    pub round_number: RoundNumber,
}

impl SessionEpoch {
    /// Sentinel epoch used by handshake messages that fire before the
    /// match id is computed. Battle 0 is never stamped by `LocalEpoch`;
    /// dispatchers key pre-match behavior off the zero `match_id`.
    pub const SENTINEL: Self = SessionEpoch {
        match_id: [0u8; 16],
        battle_number: BattleNumber(1),
        round_number: RoundNumber(0),
    };

    pub fn new(match_id: [u8; 16], battle_number: BattleNumber, round_number: RoundNumber) -> Self {
        Self {
            match_id,
            battle_number,
            round_number,
        }
    }

    pub const fn established(
        match_id: [u8; 16],
        battle_number: BattleNumber,
        round_number: RoundNumber,
    ) -> Self {
        let battle_number = if battle_number.0 == 0 {
            BattleNumber(1)
        } else {
            battle_number
        };
        Self {
            match_id,
            battle_number,
            round_number,
        }
    }
}

/// What a central dispatcher does with an inbound message's epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochPolicy {
    /// Drop messages whose `match_id` differs from the current match,
    /// and whose `battle_number` doesn't match the current battle. Used
    /// for per-turn / battle-internal events.
    CurrentBattleOnly,
    /// Drop messages from a previous battle, but accept messages from
    /// the *next* battle (the slower peer may still be in battle N when
    /// the faster peer's battle-N+1 setup arrives). Used for
    /// battle-boundary setup events.
    CurrentOrNewer,
    /// Accept regardless of epoch. Used for handshake (pre-match-id) and
    /// rejoin/control-plane messages.
    Any,
}

/// The peer's current [`SessionEpoch`]. Stamped onto every outbound game
/// event; checked against every inbound epoch.
#[derive(Resource, Debug, Clone, Copy)]
pub struct LocalEpoch(pub SessionEpoch);

impl Default for LocalEpoch {
    fn default() -> Self {
        Self(SessionEpoch::SENTINEL)
    }
}

/// Returns `true` if a wire message with the given epoch should be
/// accepted under `policy` against the local peer's current epoch.
/// Returns `false` for stale-epoch drops.
pub fn try_accept_epoch(incoming: SessionEpoch, local: &LocalEpoch, policy: EpochPolicy) -> bool {
    if matches!(policy, EpochPolicy::Any) {
        return true;
    }

    // For non-Any policies, both sides must agree on the match.
    // Sentinel match_id (all zeros) is only valid for Any-policy kinds.
    if local.0.match_id == [0u8; 16] {
        // We haven't established a match yet; accept and let
        // higher-layer logic gate.
        return true;
    }
    if incoming.match_id != local.0.match_id {
        return false;
    }

    match policy {
        EpochPolicy::CurrentBattleOnly => incoming.battle_number == local.0.battle_number,
        EpochPolicy::CurrentOrNewer => incoming.battle_number >= local.0.battle_number,
        EpochPolicy::Any => true, // unreachable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(match_id: [u8; 16], battle: BattleNumber, round: RoundNumber) -> LocalEpoch {
        LocalEpoch(SessionEpoch::new(match_id, battle, round))
    }

    #[test]
    fn any_policy_accepts_sentinel() {
        let local = LocalEpoch::default();
        let inc = SessionEpoch::SENTINEL;
        assert_eq!(local.0.battle_number, BattleNumber(1));
        assert!(try_accept_epoch(inc, &local, EpochPolicy::Any));
    }

    #[test]
    fn established_epoch_clamps_zero_battle_to_first_battle() {
        let epoch = SessionEpoch::established([9u8; 16], BattleNumber(0), RoundNumber(1));
        assert_eq!(epoch.battle_number, BattleNumber(1));
    }

    #[test]
    fn current_battle_only_accepts_match() {
        let m = [1u8; 16];
        let local = local(m, BattleNumber(3), RoundNumber(5));
        let inc = SessionEpoch::new(m, BattleNumber(3), RoundNumber(5));
        assert!(try_accept_epoch(
            inc,
            &local,
            EpochPolicy::CurrentBattleOnly
        ));
    }

    #[test]
    fn current_battle_only_rejects_stale_battle() {
        let m = [1u8; 16];
        let local = local(m, BattleNumber(3), RoundNumber(5));
        let inc = SessionEpoch::new(m, BattleNumber(2), RoundNumber(9));
        assert!(!try_accept_epoch(
            inc,
            &local,
            EpochPolicy::CurrentBattleOnly
        ));
    }

    #[test]
    fn current_battle_only_rejects_future_battle() {
        let m = [1u8; 16];
        let local = local(m, BattleNumber(3), RoundNumber(5));
        let inc = SessionEpoch::new(m, BattleNumber(4), RoundNumber(1));
        assert!(!try_accept_epoch(
            inc,
            &local,
            EpochPolicy::CurrentBattleOnly
        ));
    }

    #[test]
    fn current_or_newer_accepts_future_battle() {
        let m = [1u8; 16];
        let local = local(m, BattleNumber(3), RoundNumber(5));
        let inc = SessionEpoch::new(m, BattleNumber(4), RoundNumber(1));
        // Slow peer still in battle 3; gets faster peer's battle-4 setup.
        assert!(try_accept_epoch(inc, &local, EpochPolicy::CurrentOrNewer));
    }

    #[test]
    fn current_or_newer_rejects_stale_battle() {
        let m = [1u8; 16];
        let local = local(m, BattleNumber(3), RoundNumber(5));
        let inc = SessionEpoch::new(m, BattleNumber(2), RoundNumber(9));
        assert!(!try_accept_epoch(inc, &local, EpochPolicy::CurrentOrNewer));
    }

    #[test]
    fn match_id_mismatch_drops_non_any() {
        let local = local([1u8; 16], BattleNumber(1), RoundNumber(1));
        let inc = SessionEpoch::new([2u8; 16], BattleNumber(1), RoundNumber(1));
        assert!(!try_accept_epoch(
            inc,
            &local,
            EpochPolicy::CurrentBattleOnly
        ));
        // Any-policy kinds bypass the match-id check.
        assert!(try_accept_epoch(inc, &local, EpochPolicy::Any));
    }

    #[test]
    fn pre_negotiation_local_accepts_anything() {
        // Local hasn't negotiated yet — match_id is sentinel.
        let local = LocalEpoch::default();
        assert_eq!(local.0.match_id, [0u8; 16]);
        assert_eq!(local.0.battle_number, BattleNumber(1));
        let inc = SessionEpoch::new([7u8; 16], BattleNumber(1), RoundNumber(1));
        assert!(try_accept_epoch(
            inc,
            &local,
            EpochPolicy::CurrentBattleOnly
        ));
    }

    #[test]
    fn epoch_lex_ordering() {
        let m = [1u8; 16];
        let a = SessionEpoch::new(m, BattleNumber(1), RoundNumber(1));
        let b = SessionEpoch::new(m, BattleNumber(1), RoundNumber(2));
        let c = SessionEpoch::new(m, BattleNumber(2), RoundNumber(1));
        assert!(a.battle_number < c.battle_number);
        assert!(a.battle_number == b.battle_number && a.round_number < b.round_number);
    }
}
