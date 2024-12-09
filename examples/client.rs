//! Simple example showing how separate client and server applications can connect and talk to eachother.
//! Run the `server` example first in one terminal, then this example in another terminal.

use std::{net::Ipv6Addr, sync::Arc, time::Duration};

use bevy_app::{App, AppExit, ScheduleRunnerPlugin, Startup, Update};
use bevy_ecs::{
    event::EventWriter,
    observer::Trigger,
    schedule::IntoSystemConfigs,
    system::{Commands, Query, Res, ResMut, Resource},
};
use bevy_quicsilver::{
    ClientConfig, ConnectingError, Connection, ConnectionDrained, ConnectionError,
    ConnectionEstablished, Endpoint, EndpointBundle, QuicPlugin, RecvError, StreamId, VarInt,
};
use bevy_state::{
    app::{AppExtStates, StatesPlugin},
    prelude::in_state,
    state::{NextState, OnEnter, States},
};
use rustls::{pki_types::CertificateDer, RootCertStore};

#[derive(Default, Debug, Clone, PartialEq, Eq, Hash, States)]
enum State {
    #[default]
    Connecting,
    SendDatagrams,
    SpawnStreams,
    RecvStream,
    Closing,
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
        .add_observer(connecting_error)
        .add_observer(connection_established)
        .add_observer(connection_error)
        .add_systems(OnEnter(State::SendDatagrams), send_datagrams)
        .add_systems(OnEnter(State::SpawnStreams), spawn_streams)
        .add_systems(Update, recv_stream.run_if(in_state(State::RecvStream)))
        .add_systems(OnEnter(State::Closing), close_connection)
        .add_observer(exit_app)
        .run()
}

fn spawn_endpoint(mut commands: Commands) {
    let roots = read_crypto();

    // Use a wildcard IP and port for the local socket address
    commands.spawn(
        EndpointBundle::new_client(
            (Ipv6Addr::UNSPECIFIED, 0).into(),
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
    let path = dirs.data_local_dir().join("simple");

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

    // Connect to localhost as the server example should also be running on the same local machine
    // We know the server port number is hardcoded to 4433, so we can do the same here
    let connection = endpoint
        .connect((Ipv6Addr::LOCALHOST, 4433).into(), "localhost")
        .unwrap();

    // When a new connection is spawned and it has not yet been fully established,
    // you cannot query it with the `Connection` query parameter, and must use the `Connecting` query parameter instead
    commands.spawn(connection);
    println!("Connecting to server...");
}

fn connecting_error(trigger: Trigger<ConnectingError>) {
    // `ConnectingError` is triggered when a new connection fails to be fully established
    match trigger.event() {
        ConnectingError::Lost(e) => panic!("Failed to connect: {}", e),
        ConnectingError::IoError(e) => panic!("I/O error: {}", e),
    }
}

fn connection_established(_: Trigger<ConnectionEstablished>, mut state: ResMut<NextState<State>>) {
    // The `ConnectionEstablished` observer trigger indicates that a connection has been fully established,
    // and must now be queried for with `Connection` rather than `Connecting`
    state.set(State::SendDatagrams);
    println!("Connection established!");
}

fn connection_error(trigger: Trigger<ConnectionError>) {
    // `ConnectionError` is triggered when a connection dies unexpectedly
    match trigger.event() {
        ConnectionError::Lost(e) => panic!("Connection lost: {}", e),
        ConnectionError::IoError(e) => panic!("I/O error: {}", e),
    }
}

fn send_datagrams(mut connection: Query<&mut Connection>, mut state: ResMut<NextState<State>>) {
    // Datagrams are message-based, unreliable and unordered packets of data, comparable to UDP.
    // They may be lost or delivered out of order, but are still a useful tool for information that is quickly outdated
    let mut connection = connection.get_single_mut().unwrap();
    for i in 0..10 {
        let data = format!("Datagram #{i}");
        connection.send_datagram(data.into()).unwrap();
    }
    state.set(State::SpawnStreams);
}

fn spawn_streams(
    mut commands: Commands,
    mut connection: Query<&mut Connection>,
    mut state: ResMut<NextState<State>>,
) {
    // Streams are reliable and ordered data streams, comparable to TCP.
    // All data written to a stream will be delivered to the peer, reliably and in-order,
    // however no automatic framing is performed
    let mut connection = connection.get_single_mut().unwrap();
    let stream = connection.open_bi().unwrap();
    let mut send = connection.send_stream(stream).unwrap();
    let data = "Client Stream Data";
    send.write_all(data.as_bytes()).unwrap();
    send.finish().unwrap();
    commands.insert_resource(Stream(stream));
    state.set(State::RecvStream);
}

fn recv_stream(
    mut connection: Query<&mut Connection>,
    id: Res<Stream>,
    mut state: ResMut<NextState<State>>,
) {
    let mut connection = connection.get_single_mut().unwrap();
    let mut recv = connection.recv_stream(id.0).unwrap();

    loop {
        match recv.read_chunk(usize::MAX, true) {
            // Successfully read some data from the stream
            Ok(Some(chunk)) => {
                // Recieved chunks do not correspond to peer writes, so cannot be used for framing
                let data = String::from_utf8_lossy(&chunk.bytes);
                println!("Received from server: '{}'", data);
            }
            // The stream was finished by the server (In this example we know this will always happen)
            Ok(None) => {
                state.set(State::Closing);
                break;
            }
            // We have read all currently available data, and must wait for more to arrive
            Err(RecvError::Blocked) => break,
            // This would happen if we tried to use the stream again after it had been stopped, finished or reset,
            // or if we tried to use a stream that doesn't exist
            Err(e @ RecvError::ClosedStream) => panic!("{e}"),
            // This can only happen if we do unordered reads, which we don't
            Err(RecvError::IllegalOrderedRead) => unreachable!(),
            // The stream was reset by the server (In this example we know that doesn't happen)
            Err(e @ RecvError::Reset(_)) => panic!("{e}"),
        };
    }
}

fn close_connection(mut connection: Query<&mut Connection>) {
    // When closing a connection, you must wait a bit for pending data to be flushed,
    // and for the peer to be informed that the connection is closing
    println!("Closing");
    let mut connection = connection.get_single_mut().unwrap();
    connection.close(VarInt::from_u32(0), "Closing".into());
}

fn exit_app(_: Trigger<ConnectionDrained>, mut exit: EventWriter<AppExit>) {
    // The `ConnectionDrained` observer trigger indicates that a connection has finished closing,
    // and does not need to be kept around any longer.
    // When this trigger is activated, the connection still exists and is still queryable with the
    // `Connection` query parameter, but will be despawned immediately after the observer finishes running.
    exit.send(AppExit::Success);
}
