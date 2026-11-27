//! Platform bridge interfaces and offline implementations for desktop Bevy apps.

use std::{
    marker::PhantomData,
    sync::{Mutex, PoisonError},
};

use bevy::prelude::{App, Plugin, Resource};
pub use monetization_policy::{AdPermit, IapProof};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HapticCue {
    Tick,
    /// Application-supplied pulse intensity in thousandths (1–1000).
    Pulse(u16),
    Impact,
}

pub trait Haptics: Send + Sync {
    fn play(&self, cue: HapticCue);
}

pub trait Billing: Send + Sync {
    /// Returns an unverified store proof, if a purchase completed.
    fn purchase(&self, sku: &str) -> Option<IapProof>;
}

pub trait PushTokens: Send + Sync {
    fn token(&self) -> Option<String>;
}

pub trait SignIn: Send + Sync {
    fn sign_in(&self) -> Option<String>;
}

pub trait AdMediation<P>: Send + Sync {
    /// Consumes the permit on every attempt, even if no ad is shown.
    fn show_ad(&self, permit: AdPermit<P>) -> bool;
}

/// Records cues when constructed with `Default`. Use [`Self::silent`] to
/// discard them, as the desktop plugin does.
pub struct MockHaptics(Mutex<Option<Vec<HapticCue>>>);

impl Default for MockHaptics {
    fn default() -> Self {
        Self(Mutex::new(Some(Vec::new())))
    }
}

impl MockHaptics {
    #[must_use]
    pub const fn silent() -> Self {
        Self(Mutex::new(None))
    }

    #[must_use]
    pub fn take_cues(&self) -> Vec<HapticCue> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }
}

impl Haptics for MockHaptics {
    fn play(&self, cue: HapticCue) {
        if let Some(cues) = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            cues.push(cue);
        }
    }
}

pub struct MockBilling;

impl Billing for MockBilling {
    fn purchase(&self, _sku: &str) -> Option<IapProof> {
        None
    }
}

pub struct MockPushTokens;

impl PushTokens for MockPushTokens {
    fn token(&self) -> Option<String> {
        None
    }
}

pub struct MockSignIn;

impl SignIn for MockSignIn {
    fn sign_in(&self) -> Option<String> {
        None
    }
}

pub struct MockAdMediation;

impl<P> AdMediation<P> for MockAdMediation {
    fn show_ad(&self, _permit: AdPermit<P>) -> bool {
        false
    }
}

/// Replace any bridge with the corresponding implementation for your platform.
#[derive(Resource)]
pub struct PlatformBridges<P: 'static> {
    pub haptics: Box<dyn Haptics>,
    pub billing: Box<dyn Billing>,
    pub push_tokens: Box<dyn PushTokens>,
    pub sign_in: Box<dyn SignIn>,
    pub ads: Box<dyn AdMediation<P>>,
}

impl<P: 'static> Default for PlatformBridges<P> {
    fn default() -> Self {
        Self {
            haptics: Box::new(MockHaptics::silent()),
            billing: Box::new(MockBilling),
            push_tokens: Box::new(MockPushTokens),
            sign_in: Box::new(MockSignIn),
            ads: Box::new(MockAdMediation),
        }
    }
}

/// Installs desktop defaults unless the app has already supplied bridges.
pub struct DesktopBridgePlugin<P>(PhantomData<fn() -> P>);

impl<P> Default for DesktopBridgePlugin<P> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<P: 'static> Plugin for DesktopBridgePlugin<P> {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlatformBridges<P>>();
    }
}
