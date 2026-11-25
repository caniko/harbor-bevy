use bevy::prelude::*;
use bevy_replicated::Replicated;

#[derive(Replicated, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[replicated(component_id = 9201)]
struct MyReplicatedComponent(u32);

#[derive(Resource)]
struct Target(Entity);

fn good_system(
    target: Res<Target>,
    mut replicated: bevy::ecs::system::MediatedMut<MyReplicatedComponent>,
) {
    replicated
        .modify(target.0, |component| {
            component.0 = 2;
        })
        .expect("target has MyReplicatedComponent");
}

fn main() {
    let mut app = App::new();
    app.add_systems(Update, good_system);
}
