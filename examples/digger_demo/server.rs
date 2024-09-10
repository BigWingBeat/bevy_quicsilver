use bevy::prelude::{Commands, Resource};
use bevy_app::{App, Plugin};
use bevy_state::state::OnEnter;

use crate::AppState;

pub(super) struct ServerPlugin;

impl Plugin for ServerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ServerPassword>()
            .init_resource::<EditPermissionMode>()
            .add_systems(OnEnter(AppState::Server), start_server);
    }
}

#[derive(Resource, Default)]
pub struct ServerPassword(String);

impl From<String> for ServerPassword {
    fn from(password: String) -> Self {
        Self(password)
    }
}

/// Permission mode for controlling which clients can modify the game world. The server host can always modify the world.
#[derive(Resource, Default)]
pub enum EditPermissionMode {
    /// Only clients *not* in the list can modify the game world.
    /// As this is the default and the list starts empty, by default everyone can modify
    #[default]
    Blacklist,
    /// Only clients in the list can modify the game world.
    Whitelist,
}

fn start_server(mut commands: Commands) {}
