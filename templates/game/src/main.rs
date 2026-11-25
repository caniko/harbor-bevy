use bevy::prelude::*;
use harbor_bevy::{HarborCorePlugin, HarborUiPlugin};

fn configure_game(app: &mut App) {
    app.add_plugins((HarborCorePlugin, HarborUiPlugin))
        .add_systems(Startup, |mut commands: Commands| {
            commands.spawn(Camera2d);
        });
}

fn main() {
    let mut app = App::new();
    app.add_plugins(DefaultPlugins);
    configure_game(&mut app);
    app.run();
}

#[cfg(test)]
#[test]
fn starts_and_steps_without_a_window() {
    use bevy::diagnostic::FrameCount;

    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    configure_game(&mut app);
    app.update();
    app.update();

    let world = app.world_mut();
    assert_eq!(world.query::<&Camera2d>().iter(world).count(), 1);
    assert_eq!(world.resource::<FrameCount>().0, 2);
}
