use bevy_replicated_macros::ReplicatedResource;

#[derive(ReplicatedResource)]
#[replicated(garbage)]
struct GarbageResourceAttr;

fn main() {}
