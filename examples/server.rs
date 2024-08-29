//! Simple example showing how separate client and server applications can connect and talk to eachother.
//! Run this example first in one terminal, then the `client` example in another terminal.

use std::{net::Ipv6Addr, time::Duration};

use bevy_app::{App, AppExit, ScheduleRunnerPlugin, Startup, Update};
use bevy_ecs::{
    component::Component,
    event::EventWriter,
    observer::Trigger,
    query::Added,
    system::{Commands, Query},
};
use bevy_quicsilver::{
    Connecting, ConnectingError, Connection, ConnectionError, ConnectionEstablished,
    EndpointBundle, Incoming, IncomingResponse, NewIncoming, QuicPlugin, ServerConfig, StreamId,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

#[derive(Component)]
enum ClientState {
    WaitingForStream,
    GotStream(StreamId),
    Receiving(StreamId),
}

fn main() -> AppExit {
    App::new()
        .add_plugins((
            ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(1.0 / 60.0)),
            QuicPlugin,
        ))
        .add_systems(Startup, spawn_endpoint)
        .add_systems(Update, handle_clients)
        .observe(accept_connections)
        .observe(connecting_error)
        .observe(connection_established)
        .observe(connection_error)
        .run()
}

fn spawn_endpoint(mut commands: Commands) {
    let (cert, key) = init_crypto();

    // Use a wildcard IP and a hardcoded port number for the local socket.
    // To be able to connect to us, clients need to know this machine's IP address,
    // or a domain name that resolves to this machines IP, as well as the port number we specify here.
    // The IP specified here does not need to be known to the client.
    // If we specified 0 as the port, the OS would assign some random port number.
    // For the sake of this example, the port number is hardcoded
    commands.spawn(
        EndpointBundle::new_server(
            (Ipv6Addr::UNSPECIFIED, 4433).into(),
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
    trigger: Trigger<NewIncoming>,
    mut commands: Commands,
    new_connections: Query<&Incoming, Added<Incoming>>,
    mut new_connection_responses: EventWriter<IncomingResponse>,
) {
    // When a client tries to connect it starts as an `Incoming` that we must respond to.
    // Here we unconditionally accept connections, but you can also reject or ignore connections,
    // or tell clients to retry with address validation
    let entity = trigger.entity();
    let incoming = new_connections.get(entity).unwrap();
    println!("Client connecting from {}", incoming.remote_address());
    new_connection_responses.send(IncomingResponse::accept(entity));
    commands
        .entity(entity)
        .insert(ClientState::WaitingForStream);
}

fn connecting_error(trigger: Trigger<ConnectingError>, connecting: Query<Connecting>) {
    let connecting = connecting.get(trigger.entity()).unwrap();
    let address = connecting.remote_address();
    match trigger.event() {
        ConnectingError::Lost(e) => println!("Client {address} failed to connect: {e}"),
        ConnectingError::IoError(e) => println!("I/O error: {e}"),
    }
}

fn connection_established(trigger: Trigger<ConnectionEstablished>, connection: Query<Connection>) {
    let connection = connection.get(trigger.entity()).unwrap();
    let address = connection.remote_address();
    println!("Connection established with client {address}");
}

fn connection_error(trigger: Trigger<ConnectionError>, connection: Query<Connection>) {
    let connection = connection.get(trigger.entity()).unwrap();
    let address = connection.remote_address();
    match trigger.event() {
        ConnectionError::Lost(e) => println!("Client {address} disconnected: {e}"),
        ConnectionError::IoError(e) => println!("I/O error: {e}"),
    }
}

fn handle_clients(mut connection: Query<(Connection, &mut ClientState)>) {
    for (mut connection, mut state) in connection.iter_mut() {
        let address = connection.remote_address();

        while let Some(bytes) = connection.read_datagram() {
            let data = String::from_utf8_lossy(&bytes);
            println!("Received datagram from {address}: '{data}'");
        }

        match *state {
            ClientState::WaitingForStream => {
                if let Some(stream) = connection.accept_bi() {
                    *state = ClientState::GotStream(stream);
                }
            }
            ClientState::GotStream(stream) => {
                let mut send = connection.send_stream(stream).unwrap();
                let data = "Server Stream Data";
                send.write_all(data.as_bytes()).unwrap();
                send.finish().unwrap();
                *state = ClientState::Receiving(stream);
            }
            ClientState::Receiving(stream) => {
                let mut recv = connection.recv_stream(stream).unwrap();

                while let Ok(Some(chunk)) = recv.read_chunk(usize::MAX, true) {
                    // Recieved chunks do not correspond to peer writes, so cannot be used for framing
                    let data = String::from_utf8_lossy(&chunk.bytes);
                    println!("Received from {address}: '{}'", data);
                }
            }
        }
    }
}
