use std::{
    net::{Ipv6Addr, ToSocketAddrs},
    sync::Arc,
    time::Duration,
};

use bevy_app::{App, AppExit, ScheduleRunnerPlugin, Startup, Update};
use bevy_ecs::{
    event::EventReader,
    schedule::IntoSystemConfigs,
    system::{Commands, Query},
};
use bevy_quicsilver::{
    connection::ConnectionEstablished, endpoint::EndpointBundle, Endpoint, EntityError, QuicPlugin,
};
use quinn_proto::ClientConfig;
use rustls::{pki_types::CertificateDer, RootCertStore};

fn main() -> AppExit {
    App::new()
        .add_plugins((
            ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(1.0 / 60.0)),
            QuicPlugin,
        ))
        .add_systems(Startup, (spawn_endpoint, connect_to_server).chain())
        .add_systems(Update, handle_connection_result)
        .run()
}

fn spawn_endpoint(mut commands: Commands) {
    let roots = read_crypto();

    commands.spawn(
        EndpointBundle::new_client(
            (Ipv6Addr::LOCALHOST, 0).into(),
            Some(ClientConfig::with_root_certificates(Arc::new(roots)).unwrap()),
        )
        .unwrap(),
    );
}

/// For the sake of this example, the server generates a self-signed certificate and writes it to disk
/// at a well-known location, which is then read and trusted by the client for encryption.
///
/// In real applications, the client and server will be running on physically separate machines,
/// so instead of this the server will have to use a certificate that is signed by a trusted certificate authority,
/// or the client will have to implement either verification skipping (insecure!) or trust-on-first-use verification.
fn read_crypto() -> RootCertStore {
    let dirs = directories::ProjectDirs::from("org", "bevy_quicsilver", "bevy_quicsilver examples")
        .unwrap();
    let path = dirs.data_local_dir();

    let cert_path = path.join("cert.der");

    let cert = match std::fs::read(cert_path) {
        Ok(cert) => CertificateDer::from(cert),
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => panic!(
            "Failed to read certificate: {e}. Did you run the corresponding server example first?"
        ),
        Err(e) => panic!("Failed to read certificate: {e}"),
    };

    let mut roots = RootCertStore::empty();
    roots.add(cert).unwrap();
    roots
}

fn connect_to_server(mut commands: Commands, mut endpoint: Query<Endpoint>) {
    let mut endpoint = endpoint.get_single_mut().unwrap();

    // We know the server port number is hardcoded to 4433, so we can do the same here
    let connection = endpoint
        .connect(
            ("localhost", 4433)
                .to_socket_addrs()
                .unwrap()
                .next()
                .unwrap(),
            "localhost",
        )
        .unwrap();
    commands.spawn(connection);
    println!("Connecting to server...");
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
