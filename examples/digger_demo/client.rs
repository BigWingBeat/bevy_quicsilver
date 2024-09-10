use std::net::{Ipv6Addr, SocketAddr};

use bevy::prelude::{Resource, World};
use bevy_app::{App, Plugin};
use bevy_quicsilver::{Endpoint, EndpointBundle};
use bevy_state::state::OnEnter;

use crate::{crypto::trust_on_first_use_config, AppState, CERT_NAME, PORT};

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

fn start_client(world: &mut World) {
    let endpoint = EndpointBundle::new_client(
        (Ipv6Addr::UNSPECIFIED, 0).into(),
        Some(trust_on_first_use_config()),
    )
    .unwrap();

    let address = SocketAddr::new(world.resource::<ServerAddress>().0.parse().unwrap(), PORT);
    world.spawn(endpoint);
    let mut endpoint = world.query::<Endpoint>().get_single_mut(world).unwrap();
    let connection = endpoint.connect(address, CERT_NAME).unwrap();
    world.spawn(connection);
}
