use bevy::prelude::*;
use bevy_replicated::Replicated;

#[derive(Replicated, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[replicated(component_id = 9101)]
struct MyReplicatedComponent(u32);

fn main() {
    let mut world = World::new();
    let entity = world.spawn(MyReplicatedComponent(1)).id();
    let mut component = world
        .get_mut::<MyReplicatedComponent>(entity)
        .expect("component exists");
    component.0 = 2;
}
