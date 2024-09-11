use std::net::{Ipv6Addr, SocketAddr};

use bevy::prelude::{error, info, Resource, Trigger, World};
use bevy_app::{App, Plugin};
use bevy_quicsilver::{
    Connecting, ConnectingError, ConnectionError, ConnectionEstablished, Endpoint, EndpointBundle,
    EndpointError, IncomingError,
};
use bevy_state::state::OnEnter;

use crate::{crypto::trust_on_first_use_config, AppState, CERT_NAME, PORT};

pub(super) struct ClientPlugin;

impl Plugin for ClientPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ServerAddress>()
            .add_systems(OnEnter(AppState::Client), start_client)
            .observe(endpoint_error)
            .observe(connecting_error)
            .observe(incoming_error)
            .observe(connection_error)
            .observe(on_connected);
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
    let address = SocketAddr::new(world.resource::<ServerAddress>().0.parse().unwrap(), PORT);

    let endpoint = EndpointBundle::new_client(
        (Ipv6Addr::UNSPECIFIED, 0).into(),
        Some(trust_on_first_use_config()),
    )
    .unwrap();

    world.spawn(endpoint);
    let mut endpoint = world.query::<Endpoint>().get_single_mut(world).unwrap();
    let connection = endpoint.connect(address, CERT_NAME).unwrap();
    world.spawn(connection);
}

fn endpoint_error(error: Trigger<EndpointError>) {
    error!("{}", error.event());
}

fn connecting_error(error: Trigger<ConnectingError>) {
    error!("{}", error.event());
}

fn incoming_error(error: Trigger<IncomingError>) {
    error!("{}", error.event());
}

fn connection_error(error: Trigger<ConnectionError>) {
    error!("{}", error.event());
}

fn on_connected(_: Trigger<ConnectionEstablished>) {
    //
}
