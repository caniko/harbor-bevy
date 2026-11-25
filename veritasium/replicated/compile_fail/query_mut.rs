use bevy::prelude::*;
use bevy_replicated::Replicated;

#[derive(Replicated, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[replicated(component_id = 9102)]
struct MyReplicatedComponent(u32);

fn bad_system(mut query: Query<&mut MyReplicatedComponent>) {
    for mut component in &mut query {
        component.0 = 2;
    }
}

fn main() {
    let mut app = App::new();
    app.add_systems(Update, bad_system);
}
