use bevy::prelude::*;
use bevy_quicsilver::QuicPlugin;
use bevy_simple_text_input::TextInputPlugin;
use menu::MenuPlugin;

mod menu;

fn main() -> AppExit {
    App::new()
        .add_plugins((DefaultPlugins, TextInputPlugin, QuicPlugin, MenuPlugin))
        .add_systems(Startup, startup)
        .run()
}

fn startup(world: &mut World) {
    world.spawn(Camera2dBundle::default());
}
