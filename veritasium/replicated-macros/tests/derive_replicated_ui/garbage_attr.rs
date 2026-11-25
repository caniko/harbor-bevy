use bevy_replicated_macros::Replicated;

#[derive(Replicated)]
#[replicated(garbage)]
struct GarbageAttr;

fn main() {}
