//! One push per local day, with optional quiet hours and engagement backoff.
#![no_std]

/// Back off after this many consecutive delivery days without a click.
pub const NOTIFICATION_QUIET_BACKOFF_DAYS: u8 = 3;

const MINUTES_PER_DAY: u16 = 24 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct DeliveryId(pub [u8; 16]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct PendingPushDelivery {
    /// Must uniquely identify the delivery within this policy's scope.
    pub delivery_id: DeliveryId,
    pub sent_local_day: u32,
    pub sent_utc_ms: i64,
}

/// A daily local-time window, inclusive at the start and exclusive at the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuietHours {
    start: u16,
    end: u16,
}

impl QuietHours {
    /// Minutes since local midnight. Returns `None` for invalid or equal bounds.
    #[must_use]
    pub const fn new(start: u16, end: u16) -> Option<Self> {
        if start >= MINUTES_PER_DAY || end >= MINUTES_PER_DAY || start == end {
            None
        } else {
            Some(Self { start, end })
        }
    }

    #[must_use]
    pub const fn contains(self, local_minute: u16) -> bool {
        local_minute < MINUTES_PER_DAY
            && if self.start < self.end {
                self.start <= local_minute && local_minute < self.end
            } else {
                self.start <= local_minute || local_minute < self.end
            }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryDecision {
    Allowed,
    OptedOut,
    InvalidLocalMinute,
    DailyCap,
    QuietHours,
    QuietBackoff,
}

/// Per-recipient policy. The caller supplies local calendar dates and times.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct NotificationPolicyState {
    opted_in: bool,
    last_sent_local_day: Option<u32>,
    last_sent_utc_ms: Option<i64>,
    pending_unacked_delivery: Option<PendingPushDelivery>,
    // Misses from earlier deliveries; the pending one counts after its send day.
    consecutive_unclicked_days: u8,
}

impl NotificationPolicyState {
    #[must_use]
    pub const fn opted_in(&self) -> bool {
        self.opted_in
    }

    /// Opting out stops delivery without clearing the daily cap or click history.
    pub const fn set_opted_in(&mut self, opted_in: bool) {
        self.opted_in = opted_in;
    }

    #[must_use]
    pub const fn last_sent(&self) -> Option<(u32, i64)> {
        match (self.last_sent_local_day, self.last_sent_utc_ms) {
            (Some(day), Some(utc_ms)) => Some((day, utc_ms)),
            _ => None,
        }
    }

    #[must_use]
    pub const fn pending_delivery(&self) -> Option<PendingPushDelivery> {
        self.pending_unacked_delivery
    }

    /// Counts delivery days, not the number of days elapsed since a send.
    #[must_use]
    pub const fn unclicked_days(&self, local_day: u32) -> u8 {
        match self.pending_unacked_delivery {
            Some(pending) if pending.sent_local_day < local_day => {
                self.consecutive_unclicked_days.saturating_add(1)
            }
            _ => self.consecutive_unclicked_days,
        }
    }

    #[must_use]
    pub const fn is_quiet_paused(&self, local_day: u32) -> bool {
        self.unclicked_days(local_day) >= NOTIFICATION_QUIET_BACKOFF_DAYS
    }

    /// Checks eligibility without recording a send. `local_minute` is zero-based.
    #[must_use]
    pub const fn delivery_decision(
        &self,
        local_day: u32,
        local_minute: u16,
        quiet_hours: Option<QuietHours>,
    ) -> DeliveryDecision {
        if !self.opted_in {
            return DeliveryDecision::OptedOut;
        }
        if local_minute >= MINUTES_PER_DAY {
            return DeliveryDecision::InvalidLocalMinute;
        }
        if matches!(self.last_sent_local_day, Some(day) if local_day <= day) {
            return DeliveryDecision::DailyCap;
        }
        if self.is_quiet_paused(local_day) {
            return DeliveryDecision::QuietBackoff;
        }
        if matches!(quiet_hours, Some(hours) if hours.contains(local_minute)) {
            return DeliveryDecision::QuietHours;
        }
        DeliveryDecision::Allowed
    }

    /// Checks and records a delivery as a single state transition. Denials do not
    /// change state. Call at the dispatch boundary to reserve the day's send.
    pub const fn record_delivery(
        &mut self,
        delivery: PendingPushDelivery,
        local_minute: u16,
        quiet_hours: Option<QuietHours>,
    ) -> DeliveryDecision {
        let decision = self.delivery_decision(delivery.sent_local_day, local_minute, quiet_hours);
        if matches!(decision, DeliveryDecision::Allowed) {
            self.consecutive_unclicked_days = self.unclicked_days(delivery.sent_local_day);
            self.last_sent_local_day = Some(delivery.sent_local_day);
            self.last_sent_utc_ms = Some(delivery.sent_utc_ms);
            self.pending_unacked_delivery = Some(delivery);
        }
        decision
    }

    /// Clears backoff only when the click matches the latest unacknowledged delivery.
    /// Returns false for stale or duplicate acknowledgements.
    pub fn acknowledge(&mut self, delivery_id: DeliveryId) -> bool {
        if self
            .pending_unacked_delivery
            .is_some_and(|pending| pending.delivery_id == delivery_id)
        {
            self.pending_unacked_delivery = None;
            self.consecutive_unclicked_days = 0;
            return true;
        }
        false
    }
}
