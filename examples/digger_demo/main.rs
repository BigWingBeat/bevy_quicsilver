use bevy::prelude::*;
use bevy_quicsilver::QuicPlugin;
use client::ClientPlugin;
use menu::MenuPlugin;
use server::ServerPlugin;

mod client;
mod crypto;
mod menu;
mod server;

fn main() -> AppExit {
    App::new()
        .add_plugins((
            DefaultPlugins,
            QuicPlugin,
            ClientPlugin,
            ServerPlugin,
            MenuPlugin,
        ))
        .add_systems(Startup, startup)
        .run()
}

fn startup(world: &mut World) {
    world.spawn(Camera2dBundle::default());
}
