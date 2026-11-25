use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parental_controls::{
    AgeBand, AttemptLog, Clock, MemoryVault, PinError, PinVault, PlatformGuidance,
    ProtectedSettings, ProtectionState, disable_online, enable_online, from_local_band,
    from_platform, hash_pin, reset_forgotten_pin, set_diagnostics, verify_pin,
};

struct Frozen(SystemTime);

impl Clock for Frozen {
    fn now(&self) -> SystemTime {
        self.0
    }
}

fn clock() -> Frozen {
    Frozen(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
}

#[test]
fn unusable_platform_is_pending() {
    let state = from_platform(PlatformGuidance {
        lock_enabled: false,
        feature_community_blocked: false,
        usable: false,
    });
    assert_eq!(state, ProtectionState::PendingLocalBand);
    assert_eq!(state.age_band(), AgeBand::TenToTwelve);
    assert!(!state.online_allowed(&ProtectedSettings::default()));
    assert!(!state.diagnostics_allowed(&ProtectedSettings {
        diagnostics: true,
        ..ProtectedSettings::default()
    }));
}

#[test]
fn family_view_lock_is_child_offline() {
    let state = from_platform(PlatformGuidance {
        lock_enabled: true,
        feature_community_blocked: false,
        usable: true,
    });
    assert_eq!(state, ProtectionState::ChildOnlineDisabled);
}

#[test]
fn usable_unlocked_platform_is_adult() {
    let state = from_platform(PlatformGuidance {
        lock_enabled: false,
        feature_community_blocked: false,
        usable: true,
    });
    assert_eq!(state, ProtectionState::Adult);
}

#[test]
fn child_defaults_online_off() {
    let state = from_local_band(AgeBand::TenToTwelve);
    let settings = ProtectedSettings::default();
    assert_eq!(state, ProtectionState::ChildOnlineDisabled);
    assert!(!settings.online_play);
    assert!(!settings.social_features);
    assert!(!settings.diagnostics);
    assert!(!state.online_allowed(&settings));
    assert!(!state.diagnostics_allowed(&settings));
}

#[test]
fn pin_enable_online_leaves_diagnostics_off() {
    let hash = hash_pin("1234").unwrap();
    let mut settings = ProtectedSettings::default();
    let mut log = AttemptLog::default();
    let state = enable_online(
        ProtectionState::ChildOnlineDisabled,
        &mut settings,
        "1234",
        &hash,
        &clock(),
        &mut log,
    )
    .unwrap();
    assert_eq!(state, ProtectionState::ChildOnlineParentEnabled);
    assert!(settings.online_play);
    assert!(!settings.diagnostics);
    assert!(state.online_allowed(&settings));
    assert!(!state.diagnostics_allowed(&settings));
    assert_eq!(
        set_diagnostics(state, &mut settings, true),
        Err(PinError::DiagnosticsForbidden)
    );
}

#[test]
fn five_failures_lock() {
    let hash = hash_pin("4242").unwrap();
    let mut log = AttemptLog::default();
    for _ in 0..5 {
        assert_eq!(
            verify_pin("0000", &hash, &clock(), &mut log),
            Err(PinError::Mismatch)
        );
    }
    assert_eq!(
        verify_pin("4242", &hash, &clock(), &mut log),
        Err(PinError::Locked)
    );
}

#[test]
fn success_clears_failure_window() {
    let hash = hash_pin("9876").unwrap();
    let mut log = AttemptLog::default();
    assert_eq!(
        verify_pin("0000", &hash, &clock(), &mut log),
        Err(PinError::Mismatch)
    );
    verify_pin("9876", &hash, &clock(), &mut log).unwrap();
    verify_pin("9876", &hash, &clock(), &mut log).unwrap();
}

#[test]
fn reset_clears_pin_and_forbids_diagnostics() {
    let mut vault = MemoryVault::default();
    vault.set(hash_pin("5555").unwrap());
    let mut settings = ProtectedSettings {
        online_play: true,
        social_features: true,
        diagnostics: true,
    };
    let mut log = AttemptLog::from_millis(vec![1, 2, 3]);
    let state = reset_forgotten_pin(&mut vault, &mut settings, &mut log);
    assert_eq!(state, ProtectionState::ChildOnlineDisabled);
    assert!(vault.get().is_none());
    assert_eq!(settings, ProtectedSettings::default());
    assert_eq!(log.to_millis(), Vec::new());
    assert!(!state.online_allowed(&settings));
    assert!(!state.diagnostics_allowed(&settings));
}

#[test]
fn pending_cannot_enable_online() {
    let hash = hash_pin("1111").unwrap();
    let mut settings = ProtectedSettings::default();
    let mut log = AttemptLog::default();
    assert_eq!(
        enable_online(
            ProtectionState::PendingLocalBand,
            &mut settings,
            "1111",
            &hash,
            &clock(),
            &mut log,
        ),
        Err(PinError::WrongState)
    );
}

#[test]
fn disable_online_keeps_pin() {
    let hash = hash_pin("2222").unwrap();
    let mut settings = ProtectedSettings {
        online_play: true,
        social_features: true,
        diagnostics: false,
    };
    let state = disable_online(ProtectionState::ChildOnlineParentEnabled, &mut settings);
    assert_eq!(state, ProtectionState::ChildOnlineDisabled);
    assert!(!settings.online_play);
    assert!(!settings.social_features);
    assert_eq!(hash.as_str().chars().next(), Some('$'));
}

#[test]
fn short_pin_rejected() {
    assert!(matches!(hash_pin("12"), Err(PinError::InvalidPin)));
    assert!(matches!(hash_pin("12ab"), Err(PinError::InvalidPin)));
}

#[test]
fn adult_may_enable_diagnostics() {
    let mut settings = ProtectedSettings::default();
    set_diagnostics(ProtectionState::Adult, &mut settings, true).unwrap();
    assert!(ProtectionState::Adult.diagnostics_allowed(&settings));
}
