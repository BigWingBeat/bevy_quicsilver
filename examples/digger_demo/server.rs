use std::{net::Ipv6Addr, sync::Arc};

use bevy::prelude::{
    error, info, Added, Commands, Component, EventWriter, Query, Resource, Trigger,
};
use bevy_app::{App, Plugin};
use bevy_quicsilver::{
    ConnectingError, Connection, ConnectionError, ConnectionEstablished, EndpointBundle,
    EndpointError, Incoming, IncomingError, IncomingResponse, NewIncoming,
};
use bevy_state::state::OnEnter;
use quinn_proto::{ClientConfig, ServerConfig};
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    RootCertStore,
};

use crate::{AppState, CERT_NAME, PORT};

pub(super) struct ServerPlugin;

impl Plugin for ServerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ServerPassword>()
            .init_resource::<EditPermissionMode>()
            .add_systems(OnEnter(AppState::Server), start_server)
            .observe(accept_connections)
            .observe(endpoint_error)
            .observe(connecting_error)
            .observe(incoming_error)
            .observe(connection_error)
            .observe(on_connected);
    }
}

#[derive(Component, Default)]
enum ClientState {
    #[default]
    WaitingForPassword,
    Authenticated,
    Host,
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

fn start_server(mut commands: Commands) {
    // When hosting a server you still need a client running so the host can also play the game.
    // As the server and client both run in the same process in this case,
    // the client can entirely shortcut cert verification by just directly trusting the server certificate.
    let (cert, key) = init_crypto();

    let mut roots = RootCertStore::empty();
    roots.add(cert.clone()).unwrap();
    let client_config = ClientConfig::with_root_certificates(Arc::new(roots)).unwrap();

    let server_config = ServerConfig::with_single_cert(vec![cert], key).unwrap();

    let endpoint = EndpointBundle::new_client_host(
        (Ipv6Addr::UNSPECIFIED, PORT).into(),
        client_config,
        server_config,
    )
    .unwrap();

    commands.spawn(endpoint);
}

/// Generate a self-signed certificate and store it on disk. In this example, the client implements trust-on-first-use verification,
/// which means we can't simply re-generate the certificate each startup as that would result in the clients rejecting the new
/// certificate. Instead, we persist the certificate to disk and re-use it in order to pass the client's verification.
fn init_crypto() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let dirs = directories::ProjectDirs::from("org", "bevy_quicsilver", "bevy_quicsilver examples")
        .unwrap();
    let path = dirs.data_local_dir();

    let cert_path = path.join("cert.der");
    let key_path = path.join("key.der");

    let (cert, key) = match std::fs::read(&cert_path)
        .and_then(|cert| Ok((cert, std::fs::read(&key_path)?)))
    {
        Ok((cert, key)) => (
            CertificateDer::from(cert),
            PrivateKeyDer::try_from(key).unwrap(),
        ),
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => {
            info!("Generating self-signed certificate");
            let cert = rcgen::generate_simple_self_signed(vec![CERT_NAME.into()]).unwrap();
            let key = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
            let cert = cert.cert.into();
            std::fs::create_dir_all(path).expect("Failed to create certificate directory");
            std::fs::write(&cert_path, &cert).expect("Failed to write certificate");
            std::fs::write(&key_path, key.secret_pkcs8_der()).expect("Failed to write private key");
            (cert, key.into())
        }
        Err(e) => panic!("Failed to read certificate: {e}"),
    };

    (cert, key)
}

fn accept_connections(
    trigger: Trigger<NewIncoming>,
    new_connections: Query<&Incoming, Added<Incoming>>,
    mut new_connection_responses: EventWriter<IncomingResponse>,
) {
    let entity = trigger.entity();
    let incoming = new_connections.get(entity).unwrap();
    if incoming.remote_address_validated() {
        info!("Client connecting from {}", incoming.remote_address());
        new_connection_responses.send(IncomingResponse::accept(entity));
    } else {
        new_connection_responses.send(IncomingResponse::retry(entity));
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

fn on_connected(trigger: Trigger<ConnectionEstablished>, connection: Query<Connection>) {
    let connection = connection.get(trigger.entity()).unwrap();
    info!("Client {} finished connecting", connection.remote_address());
}
