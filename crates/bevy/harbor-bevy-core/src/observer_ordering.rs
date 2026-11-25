//! Named observer registration for Stock and Veritasium Bevy.
//!
//! With Stock Bevy, stage helpers register ordinary observers and do not impose
//! execution order. Enable `veritasium-observers` only when the consuming
//! workspace supplies a Veritasium Bevy patch. The feature does not select or
//! patch Bevy itself.

use bevy::{
    app::{App, Plugin},
    ecs::{
        event::EventPattern,
        observer::{IntoObserver, Observer},
        system::IntoObserverSystem,
    },
};

#[cfg(feature = "veritasium-observers")]
use bevy::ecs::observer::{IntoObserverConfigs, IntoObserverSetConfigs, ObserverSet};
#[cfg(not(feature = "veritasium-observers"))]
use bevy::ecs::system::IntoSystem;

/// Convert an observer system into an `Observer` carrying the given debug name.
pub trait ObserverNameExt<E: EventPattern, M>: IntoObserverSystem<E, M> + Sized {
    fn with_name(self, name: impl Into<String>) -> Observer {
        #[cfg(feature = "veritasium-observers")]
        {
            Observer::new(self).with_name(name)
        }
        #[cfg(not(feature = "veritasium-observers"))]
        {
            // Stock can name a system pipe, but not an Observer directly.
            let system = IntoObserverSystem::into_system(self);
            Observer::new(system.pipe(|| {}).with_name(name.into()))
        }
    }
}

impl<S, E: EventPattern, M> ObserverNameExt<E, M> for S where S: IntoObserverSystem<E, M> {}

/// Configure the stage chain on Veritasium; no-op on Stock Bevy.
pub struct ObserverOrderingPlugin;

macro_rules! observer_stages {
    (@order $app:ident; $first:ident, $($later:ident),+) => {
        $app.configure_observer_sets($first$(.before($later))+);
        observer_stages!(@order $app; $($later),+);
    };
    (@order $app:ident; $last:ident) => {};

    ($($stage:ident => $single:ident, $batch:ident);+ $(;)?) => {
        $(
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
            #[cfg_attr(feature = "veritasium-observers", derive(ObserverSet))]
            pub struct $stage;
        )+

        /// Register observers at named application stages.
        pub trait AppObserverOrderingExt {
            $(
                fn $single<M>(&mut self, observer: impl IntoObserver<M>) -> &mut Self;

                #[cfg(feature = "veritasium-observers")]
                fn $batch<M>(&mut self, observers: impl IntoObserverConfigs<M>) -> &mut Self;
            )+
        }

        impl AppObserverOrderingExt for App {
            $(
                fn $single<M>(&mut self, observer: impl IntoObserver<M>) -> &mut Self {
                    #[cfg(feature = "veritasium-observers")]
                    { self.add_observers(observer.in_set($stage)) }
                    #[cfg(not(feature = "veritasium-observers"))]
                    { self.add_observer(observer) }
                }

                #[cfg(feature = "veritasium-observers")]
                fn $batch<M>(&mut self, observers: impl IntoObserverConfigs<M>) -> &mut Self {
                    self.add_observers(observers.in_set($stage))
                }
            )+
        }

        impl Plugin for ObserverOrderingPlugin {
            fn build(&self, _app: &mut App) {
                // The fork expands edges between actual observers. Declare
                // direct edges so an empty stage cannot interrupt the chain.
                #[cfg(feature = "veritasium-observers")]
                observer_stages!(@order _app; $($stage),+);
            }
        }
    };
}

observer_stages! {
    ValidationObservers => add_validation_observer, add_validation_observers;
    GameplayMutationObservers => add_gameplay_mutation_observer, add_gameplay_mutation_observers;
    GameplayFollowupObservers => add_gameplay_followup_observer, add_gameplay_followup_observers;
    NetworkBridgeObservers => add_network_bridge_observer, add_network_bridge_observers;
    RecordingObservers => add_recording_observer, add_recording_observers;
    PresentationObservers => add_presentation_observer, add_presentation_observers;
    TelemetryObservers => add_telemetry_observer, add_telemetry_observers;
    DiagnosticsObservers => add_diagnostics_observer, add_diagnostics_observers;
    TestHarnessObservers => add_test_harness_observer, add_test_harness_observers;
}
