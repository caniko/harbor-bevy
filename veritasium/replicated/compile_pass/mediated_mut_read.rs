use bevy::ecs::system::MediatedMut;
use bevy::prelude::*;
use bevy_replicated::Replicated;

#[derive(Replicated, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[replicated(component_id = 9202)]
struct MyReplicatedComponent(u32);

#[derive(Resource)]
struct Target(Entity);

fn good_system(target: Res<Target>, mut replicated: MediatedMut<MyReplicatedComponent>) {
    // Reading a mediated component through `MediatedMut::get` is allowed and
    // deposits nothing. In-place mutation is closure-only (`modify`/`for_each_mut`);
    // there is no bare `get_mut`/`iter_mut` to peel a `Mut<T>` out of.
    let _ = replicated.get(target.0);
}

fn main() {
    let mut app = App::new();
    app.add_systems(Update, good_system);
}
