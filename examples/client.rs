use std::{
    net::{Ipv6Addr, ToSocketAddrs},
    sync::Arc,
    time::Duration,
};

use bevy_app::{App, AppExit, ScheduleRunnerPlugin, Startup, Update};
use bevy_ecs::{
    event::EventReader,
    schedule::IntoSystemConfigs,
    system::{Commands, Query, Res, ResMut, Resource},
};
use bevy_quicsilver::{
    connection::{Connection, ConnectionEvent, ConnectionEventType},
    endpoint::EndpointBundle,
    Endpoint, QuicPlugin,
};
use bevy_state::{
    app::{AppExtStates, StatesPlugin},
    prelude::in_state,
    state::{NextState, States},
};
use quinn_proto::{ClientConfig, StreamId};
use rustls::{pki_types::CertificateDer, RootCertStore};

#[derive(Default, Debug, Clone, PartialEq, Eq, Hash, States)]
enum State {
    #[default]
    Connecting,
    SendDatagrams,
    SpawnStreams,
    RecvStream,
}

#[derive(Debug, Resource)]
struct Stream(StreamId);

fn main() -> AppExit {
    App::new()
        .add_plugins((
            ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(1.0 / 60.0)),
            StatesPlugin,
            QuicPlugin,
        ))
        .init_state::<State>()
        .add_systems(Startup, (spawn_endpoint, connect_to_server).chain())
        .add_systems(Update, handle_connection_result)
        .add_systems(
            Update,
            send_datagrams.run_if(in_state(State::SendDatagrams)),
        )
        .add_systems(Update, spawn_streams.run_if(in_state(State::SpawnStreams)))
        .add_systems(Update, recv_stream.run_if(in_state(State::RecvStream)))
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
    mut events: EventReader<ConnectionEvent>,
    mut state: ResMut<NextState<State>>,
) {
    if let Some(event) = events.read().next() {
        match &event.event {
            ConnectionEventType::Established => {
                state.set(State::SendDatagrams);
                println!("Connection established!");
            }
            ConnectionEventType::Lost(e) => panic!("Connection lost: {}", e),
            ConnectionEventType::IoError(e) => panic!("I/O error: {}", e),
        }
    }
}

fn send_datagrams(mut connection: Query<Connection>, mut state: ResMut<NextState<State>>) {
    let mut connection = connection.get_single_mut().unwrap();
    for i in 0..10 {
        let data = format!("Datagram #{i}");
        connection.send_datagram(data.into()).unwrap();
    }
    state.set(State::SpawnStreams);
}

fn spawn_streams(
    mut commands: Commands,
    mut connection: Query<Connection>,
    mut state: ResMut<NextState<State>>,
) {
    let mut connection = connection.get_single_mut().unwrap();
    let stream = connection.open_bi().unwrap();
    let mut send = connection.send_stream(stream).unwrap();
    let data = "Stream Data";
    send.write(data.as_bytes()).unwrap();
    commands.insert_resource(Stream(stream));
    state.set(State::RecvStream);
}

fn recv_stream(mut connection: Query<Connection>, id: Res<Stream>) {
    let mut connection = connection.get_single_mut().unwrap();
    let mut recv = connection.recv_stream(id.0).unwrap();
    let mut chunks = recv.read(true).unwrap();
    while let Ok(Some(chunk)) = chunks.next(usize::MAX) {
        let data = String::from_utf8_lossy(&chunk.bytes);
        println!("Recv: '{}'", data);
    }
    let _ = chunks.finalize();
}
