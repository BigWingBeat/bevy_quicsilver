use std::net::Ipv6Addr;

use bevy::prelude::{Commands, Resource};
use bevy_app::{App, Plugin};
use bevy_quicsilver::EndpointBundle;
use bevy_state::state::OnEnter;

use crate::{crypto::trust_on_first_use_config, AppState};

pub(super) struct ClientPlugin;

impl Plugin for ClientPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ServerAddress>()
            .add_systems(OnEnter(AppState::Client), start_client);
    }
}

#[derive(Resource, Default)]
pub struct ServerAddress(String);

impl From<String> for ServerAddress {
    fn from(address: String) -> Self {
        Self(address)
    }
}

fn start_client(mut commands: Commands) {
    let endpoint = EndpointBundle::new_client(
        (Ipv6Addr::UNSPECIFIED, 0).into(),
        Some(trust_on_first_use_config()),
    )
    .unwrap();

    commands.spawn(endpoint);
}
