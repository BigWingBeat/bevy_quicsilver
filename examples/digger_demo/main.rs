use bevy::prelude::*;
use bevy_quicsilver::QuicPlugin;
use bevy_state::{app::AppExtStates, state::States};
use client::ClientPlugin;
use menu::MenuPlugin;
use server::ServerPlugin;

mod client;
mod crypto;
mod menu;
mod server;

const CERT_NAME: &str = "digger_demo";
const PORT: u16 = 5544;

fn main() -> AppExit {
    App::new()
        .add_plugins((
            DefaultPlugins,
            QuicPlugin,
            ClientPlugin,
            ServerPlugin,
            MenuPlugin,
        ))
        .init_state::<AppState>()
        .init_resource::<Username>()
        .add_systems(Startup, startup)
        .run()
}

fn startup(world: &mut World) {
    world.spawn(Camera2dBundle::default());
}

#[derive(States, Default, Debug, Hash, PartialEq, Eq, Clone)]
enum AppState {
    #[default]
    Menu,
    Client,
    Server,
}

#[derive(Event)]
struct ErrorMessage(anyhow::Error);

#[derive(Resource, Default)]
struct Username(String);

impl From<String> for Username {
    fn from(username: String) -> Self {
        Self(username)
    }
}
