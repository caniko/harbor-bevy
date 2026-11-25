use bevy::prelude::*;
use harbor_bevy_core::observer_ordering::{
    AppObserverOrderingExt, ObserverNameExt, ObserverOrderingPlugin,
};

#[derive(Event)]
struct Pulse;

#[derive(Resource, Default)]
struct Visits(Vec<u8>);

fn record(index: u8) -> impl FnMut(On<Pulse>, ResMut<Visits>) {
    move |_, mut visits| visits.0.push(index)
}

#[test]
fn all_single_observer_stages_register_named_and_unnamed_observers() {
    let mut app = App::new();
    app.add_plugins(ObserverOrderingPlugin)
        .init_resource::<Visits>()
        .add_test_harness_observer(record(9))
        .add_diagnostics_observer(record(8))
        .add_telemetry_observer(record(7))
        .add_presentation_observer(record(6))
        .add_recording_observer(record(5))
        .add_network_bridge_observer(record(4).with_name("network bridge"))
        .add_gameplay_followup_observer(record(3))
        .add_gameplay_mutation_observer(record(2))
        .add_validation_observer(record(1).with_name("validation"));
    app.world_mut().trigger(Pulse);
    let visits = app.world().resource::<Visits>();
    #[cfg(feature = "veritasium-observers")]
    assert_eq!(visits.0, [1, 2, 3, 4, 5, 6, 7, 8, 9]);
    #[cfg(not(feature = "veritasium-observers"))]
    {
        let mut visited = visits.0.clone();
        visited.sort_unstable();
        assert_eq!(visited, [1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }
}

#[cfg(feature = "veritasium-observers")]
#[test]
fn batches_can_chain_observers_within_a_stage() {
    use bevy::ecs::observer::IntoObserverConfigs;

    let mut app = App::new();
    app.add_plugins(ObserverOrderingPlugin)
        .init_resource::<Visits>()
        .add_presentation_observers(record(3))
        .add_gameplay_mutation_observers((record(1), record(2).with_name("second")).chain());
    app.world_mut().trigger(Pulse);
    assert_eq!(app.world().resource::<Visits>().0, [1, 2, 3]);
}
