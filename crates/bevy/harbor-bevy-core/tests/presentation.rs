use std::time::Duration;

use bevy::prelude::*;
use harbor_bevy_core::{
    cue_diff::{ActiveCueSet, TimedEntityReaper},
    interpolation::{Interpolatable, InterpolatePresentationSet, PoseHistory},
    observation_buffer::CommittedTickObservationBuffer,
};

#[test]
fn presentation_uses_two_fixed_snapshots_without_modifying_either() {
    let mut app = App::new();
    app.insert_resource(PoseHistory::new(Vec3::ZERO))
        .add_systems(FixedUpdate, |mut history: ResMut<PoseHistory<Vec3>>| {
            let next = *history.current() + Vec3::X * 4.;
            history.push(next);
        })
        .add_systems(
            PostUpdate,
            (|history: Res<PoseHistory<Vec3>>, mut visuals: Query<&mut Transform>| {
                for mut transform in &mut visuals {
                    transform.translation = history.interpolated(0.25);
                }
            })
            .in_set(InterpolatePresentationSet),
        )
        .add_systems(
            PostUpdate,
            (|visuals: Query<&Transform>| {
                assert_eq!(visuals.single().unwrap().translation, Vec3::X * 5.);
            })
            .after(InterpolatePresentationSet),
        );
    app.world_mut().spawn(Transform::default());
    app.world_mut().run_schedule(FixedUpdate);
    app.world_mut().run_schedule(FixedUpdate);
    app.update();
    let history = app.world().resource::<PoseHistory<Vec3>>();
    assert_eq!(*history.previous(), Vec3::X * 4.);
    assert_eq!(*history.current(), Vec3::X * 8.);
}

#[test]
fn interpolation_blends_transform_and_can_reset_discontinuities() {
    let start = Transform::IDENTITY;
    let end = Transform {
        translation: Vec3::X * 4.,
        rotation: Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
        scale: Vec3::splat(3.),
    };
    let mut history = PoseHistory::new(start);
    history.push(end);
    let middle = history.interpolated(0.5);
    assert_eq!(middle.translation, Vec3::X * 2.);
    assert_eq!(middle.scale, Vec3::splat(2.));
    assert!((middle.rotation * Vec3::X).abs_diff_eq(Vec3::new(1., 1., 0.).normalize(), 1e-6));
    assert_eq!(2_f32.interpolate(&4., 0.5), 3.);
    assert_eq!(Vec2::ZERO.interpolate(&Vec2::ONE, 0.5), Vec2::splat(0.5));
    history.reset(end);
    assert_eq!(*history.previous(), end);
    assert_eq!(*history.current(), end);
    assert!(
        history
            .interpolated(0.)
            .rotation
            .abs_diff_eq(end.rotation, 1e-6)
    );
}

#[test]
fn observations_are_first_write_wins_and_retired_in_generation_order() {
    struct Receipt(u32);
    let mut app = App::new();
    app.insert_resource(CommittedTickObservationBuffer::<Receipt>::new(2))
        .add_systems(
            FixedUpdate,
            |mut buffer: ResMut<CommittedTickObservationBuffer<Receipt>>| {
                assert!(buffer.publish(1, 99, Receipt(10)));
                assert!(!buffer.publish(1, 99, Receipt(20)));
                assert!(buffer.publish(2, 0, Receipt(30)));
                assert!(!buffer.publish(2, 1, Receipt(40)));
            },
        )
        .add_systems(
            PostUpdate,
            |mut buffer: ResMut<CommittedTickObservationBuffer<Receipt>>| {
                assert_eq!(buffer.get(1, 99).unwrap().0, 10);
                assert_eq!(buffer.len(), 2);
                buffer.retire_before(2, 0);
                assert!(buffer.get(1, 99).is_none());
                assert_eq!(buffer.get(2, 0).unwrap().0, 30);
                assert!(buffer.publish(2, 1, Receipt(40)));
            },
        );
    app.world_mut().run_schedule(FixedUpdate);
    app.update();
    let mut buffer = app
        .world_mut()
        .resource_mut::<CommittedTickObservationBuffer<Receipt>>();
    assert_eq!(buffer.capacity(), 2);
    buffer.clear();
    assert!(buffer.is_empty());
    assert!(!CommittedTickObservationBuffer::new(0).publish(0, 0, Receipt(0)));
}

#[test]
fn cue_diffs_are_sorted_deduplicated_and_end_when_input_clears() {
    let mut cues = ActiveCueSet::default();
    assert_eq!(cues.update([3, 1, 1]).started, [1, 3]);
    let diff = cues.update([2, 3, 2]);
    assert_eq!(diff.started, [2]);
    assert_eq!(diff.persisted, [3]);
    assert_eq!(diff.ended, [1]);
    let cleared = cues.update([]);
    assert!(cleared.started.is_empty());
    assert!(cleared.persisted.is_empty());
    assert_eq!(cleared.ended, [2, 3]);
}

#[test]
fn timer_reaps_even_if_playback_never_started_or_already_despawned() {
    let mut app = App::new();
    let live = app.world_mut().spawn_empty().id();
    let already_gone = app.world_mut().spawn_empty().id();
    let later = app.world_mut().spawn_empty().id();
    assert!(app.world_mut().despawn(already_gone));
    let mut reaper = TimedEntityReaper::default();
    assert!(reaper.track(1, live, Duration::from_secs(2)));
    assert!(reaper.track(2, already_gone, Duration::from_secs(2)));
    assert!(!reaper.track(1, later, Duration::ZERO));
    assert!(reaper.track(3, later, Duration::from_secs(3)));
    app.insert_resource(reaper).add_systems(
        Update,
        |mut reaper: ResMut<TimedEntityReaper<i32>>, mut commands: Commands| {
            assert_eq!(
                reaper.reap_expired(Duration::from_secs(1), &mut commands),
                0
            );
            assert_eq!(
                reaper.reap_expired(Duration::from_secs(2), &mut commands),
                2
            );
        },
    );
    app.update();
    assert!(app.world().get_entity(live).is_err());
    assert!(app.world().get_entity(later).is_ok());
    assert!(
        !app.world()
            .resource::<TimedEntityReaper<i32>>()
            .contains(&1)
    );
}
