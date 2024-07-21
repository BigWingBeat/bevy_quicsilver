//! Simple example showing how separate client and server applications can connect and talk to eachother.
//! Run this example first in one terminal, then the `client` example in another terminal.

use std::{net::Ipv6Addr, time::Duration};

use bevy_app::{App, AppExit, ScheduleRunnerPlugin, Startup, Update};
use bevy_ecs::{
    event::{EventReader, EventWriter},
    query::Added,
    system::{Commands, Query},
};
use bevy_quicsilver::{
    connection::ConnectionEstablished, endpoint::EndpointBundle, EntityError, Incoming,
    IncomingResponse, NewIncoming, QuicPlugin,
};
use quinn_proto::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

fn main() -> AppExit {
    App::new()
        .add_plugins((
            ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(1.0 / 60.0)),
            QuicPlugin,
        ))
        .add_systems(Startup, spawn_endpoint)
        .add_systems(Update, (accept_connections, handle_connection_result))
        .run()
}

fn spawn_endpoint(mut commands: Commands) {
    let (cert, key) = init_crypto();

    // Hardcoding the server port number allows you to do the same in the client app,
    // removing the need to find some way of externally communicating it to clients
    commands.spawn(
        EndpointBundle::new_server(
            (Ipv6Addr::LOCALHOST, 4433).into(),
            ServerConfig::with_single_cert(cert, key).unwrap(),
        )
        .unwrap(),
    );

    println!("Listening for incoming connections...");
}

/// For the sake of this example, the server generates a self-signed certificate and writes it to disk
/// at a well-known location, which is then read and trusted by the client for encryption.
///
/// In real applications, the client and server will be running on physically separate machines,
/// so instead of this the server will have to use a certificate that is signed by a trusted certificate authority,
/// or the client will have to implement either verification skipping (insecure!) or trust-on-first-use verification.
fn init_crypto() -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
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
            println!("Generating self-signed certificate");
            let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            let key = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
            let cert = cert.cert.into();
            std::fs::create_dir_all(path).expect("Failed to create certificate directory");
            std::fs::write(&cert_path, &cert).expect("Failed to write certificate");
            std::fs::write(&key_path, key.secret_pkcs8_der()).expect("Failed to write private key");
            (cert, key.into())
        }
        Err(e) => panic!("Failed to read certificate: {e}"),
    };

    (vec![cert], key)
}

fn accept_connections(
    new_connections: Query<&Incoming, Added<Incoming>>,
    mut new_connection_events: EventReader<NewIncoming>,
    mut new_connection_responses: EventWriter<IncomingResponse>,
) {
    for &NewIncoming(entity) in new_connection_events.read() {
        let incoming = new_connections.get(entity).unwrap();
        println!("Client connecting from {}", incoming.remote_address());
        new_connection_responses.send(IncomingResponse::accept(entity));
    }
}

fn handle_connection_result(
    mut success: EventReader<ConnectionEstablished>,
    mut error: EventReader<EntityError>,
) {
    if let Some(e) = error.read().next() {
        panic!("{}", e.error);
    }

    if success.read().next().is_some() {
        println!("Connection established!");
    }
}
