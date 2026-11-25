use bevy::prelude::*;
use bevy_replicated::Replicated;

#[derive(Replicated, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[replicated(component_id = 9103)]
struct MyReplicatedComponent(u32);

fn main() {
    let mut world = World::new();
    let entity = world.spawn(MyReplicatedComponent(1)).id();
    let mut entity = world.entity_mut(entity);
    let mut component = entity
        .get_mut::<MyReplicatedComponent>()
        .expect("component exists");
    component.0 = 2;
}
