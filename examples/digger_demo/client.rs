use std::net::{Ipv6Addr, SocketAddr};

use bevy::prelude::{error, info, Component, Query, Res, Resource, Trigger, World};
use bevy_app::{App, Plugin};
use bevy_quicsilver::{
    ConnectingError, Connection, ConnectionError, ConnectionEstablished, Endpoint, EndpointBundle,
    EndpointError, IncomingError,
};
use bevy_state::state::OnEnter;
use bincode::{DefaultOptions, Options};

use crate::{
    crypto::trust_on_first_use_config, proto::ClientHello, AppState, ErrorMessage, Password,
    Username, CERT_NAME, PORT,
};

pub(super) struct ClientPlugin;

impl Plugin for ClientPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ServerAddress>()
            .add_systems(OnEnter(AppState::Client), start_client)
            .observe(endpoint_error)
            .observe(connecting_error)
            .observe(incoming_error)
            .observe(connection_error);
    }
}

#[derive(Component)]
enum ConnectionState {
    Connecting,
    SendingHello,
    Authenticated,
}

#[derive(Resource, Default)]
pub struct ServerAddress(String);

impl From<String> for ServerAddress {
    fn from(address: String) -> Self {
        Self(address)
    }
}

fn start_client(world: &mut World) {
    let address = match world.resource::<ServerAddress>().0.parse() {
        Ok(a) => a,
        Err(e) => {
            world.trigger(ErrorMessage(anyhow::Error::from(e)));
            return;
        }
    };
    let address = SocketAddr::new(address, PORT);

    let endpoint = EndpointBundle::new_client(
        (Ipv6Addr::UNSPECIFIED, 0).into(),
        Some(trust_on_first_use_config()),
    )
    .unwrap();

    world.spawn(endpoint);
    let mut endpoint = world.query::<Endpoint>().get_single_mut(world).unwrap();
    match endpoint.connect(address, CERT_NAME) {
        Ok(connection) => {
            world
                .spawn((connection, ConnectionState::Connecting))
                .observe(on_connected);
        }
        Err(e) => world.trigger(ErrorMessage(e.into())),
    }
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

fn on_connected(
    _: Trigger<ConnectionEstablished>,
    mut connection: Query<(Connection, &mut ConnectionState)>,
    username: Res<Username>,
    password: Res<Password>,
) {
    let (mut connection, mut state) = connection.single_mut();
    assert!(matches!(*state, ConnectionState::Connecting));
    *state = ConnectionState::SendingHello;
    let hello = ClientHello {
        username: username.0.clone(),
        password: password.0.clone(),
    };
    let mut stream = connection.open_uni().unwrap();
    let serialized = DefaultOptions::new().serialize(&hello).unwrap();
    stream.write_chunk(serialized.into()).unwrap();
    stream.finish().unwrap();
    info!("Sending hello");
}
