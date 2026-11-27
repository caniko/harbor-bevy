use notification_policy::{
    DeliveryDecision, DeliveryId, NotificationPolicyState, PendingPushDelivery, QuietHours,
};

fn delivery(day: u32) -> PendingPushDelivery {
    PendingPushDelivery {
        delivery_id: DeliveryId([u8::try_from(day).unwrap(); 16]),
        sent_local_day: day,
        sent_utc_ms: i64::from(day) * 86_400_000,
    }
}

#[test]
fn daily_cap_resets_only_on_a_later_local_day() {
    use DeliveryDecision::{Allowed, DailyCap, OptedOut};
    let mut state = NotificationPolicyState::default();
    assert_eq!(state.record_delivery(delivery(10), 0, None), OptedOut);
    state.set_opted_in(true);
    assert_eq!(state.record_delivery(delivery(10), 0, None), Allowed);
    assert_eq!(state.record_delivery(delivery(10), 1, None), DailyCap);
    assert_eq!(state.record_delivery(delivery(9), 0, None), DailyCap);
    assert!(state.acknowledge(delivery(10).delivery_id));
    assert!(!state.acknowledge(delivery(10).delivery_id));
    state.set_opted_in(false);
    assert_eq!(state.record_delivery(delivery(10), 2, None), OptedOut);
    state.set_opted_in(true);
    assert_eq!(state.record_delivery(delivery(10), 2, None), DailyCap);
    assert_eq!(state.last_sent(), Some((10, delivery(10).sent_utc_ms)));
    assert_eq!(state.record_delivery(delivery(11), 0, None), Allowed);
}

#[test]
fn misses_count_per_delivery_day_and_a_matching_click_resumes_sends() {
    use DeliveryDecision::{Allowed, QuietBackoff};
    let mut state = NotificationPolicyState::default();
    state.set_opted_in(true);
    for day in [1, 20, 50] {
        assert_eq!(state.record_delivery(delivery(day), 0, None), Allowed);
    }
    assert!(!state.is_quiet_paused(50));
    assert!(state.is_quiet_paused(51));
    assert_eq!(state.record_delivery(delivery(51), 0, None), QuietBackoff);
    assert!(!state.acknowledge(delivery(20).delivery_id));
    assert_eq!(state.record_delivery(delivery(52), 0, None), QuietBackoff);
    assert!(state.acknowledge(delivery(50).delivery_id));
    assert_eq!(state.record_delivery(delivery(52), 0, None), Allowed);
}

#[test]
fn quiet_hours_wrap_midnight_and_end_exclusively() {
    let hours = QuietHours::new(22 * 60, 7 * 60).unwrap();
    let mut state = NotificationPolicyState::default();
    state.set_opted_in(true);
    for minute in [0, 7 * 60 - 1, 22 * 60, 1439] {
        assert_eq!(
            state.record_delivery(delivery(1), minute, Some(hours)),
            DeliveryDecision::QuietHours
        );
    }
    assert_eq!(
        state.record_delivery(delivery(1), 1440, Some(hours)),
        DeliveryDecision::InvalidLocalMinute
    );
    assert_eq!(
        state.record_delivery(delivery(1), 7 * 60, Some(hours)),
        DeliveryDecision::Allowed
    );
    assert!(QuietHours::new(1440, 1).is_none());
    assert!(QuietHours::new(1, 1).is_none());
    let afternoon = QuietHours::new(800, 900).unwrap();
    assert!(!afternoon.contains(799));
    assert!(afternoon.contains(800));
    assert!(!afternoon.contains(900));
}

#[cfg(any(feature = "serde", feature = "rkyv"))]
fn state_after_sends() -> NotificationPolicyState {
    let mut state = NotificationPolicyState::default();
    state.set_opted_in(true);
    state.record_delivery(delivery(1), 0, None);
    state.record_delivery(delivery(2), 0, None);
    state
}

#[cfg(feature = "serde")]
#[test]
fn serde_round_trips_delivery_state() {
    let state = state_after_sends();
    let wire = serde_json::to_string(&state).unwrap();
    assert_eq!(
        serde_json::from_str::<NotificationPolicyState>(&wire).unwrap(),
        state
    );
}

#[cfg(feature = "rkyv")]
#[test]
fn rkyv_round_trips_delivery_state() {
    use rkyv::rancor::Error;
    let state = state_after_sends();
    let bytes = rkyv::to_bytes::<Error>(&state).unwrap();
    assert_eq!(
        rkyv::from_bytes::<NotificationPolicyState, Error>(&bytes).unwrap(),
        state
    );
}
