use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Instant,
};

use bevy_ecs::{
    component::{Component, ComponentHooks, StorageType},
    entity::Entity,
    event::Event,
    query::{Has, QueryEntityError},
    system::{Commands, Query},
};
use bevy_tasks::IoTaskPool;
use crossbeam_channel::Receiver;
use hashbrown::HashMap;
use quinn_proto::{
    AcceptError, ClientConfig, ConnectionError, ConnectionEvent, ConnectionHandle, DatagramEvent,
    EndpointConfig, EndpointEvent, RetryError, ServerConfig,
};
use thiserror::Error;

use crate::{
    connection::ConnectionQuery,
    incoming::Incoming,
    socket::{UdpSocket, UdpSocketRecvDriver},
    Connecting, KeepAlive, KeepAliveEntityCommandsExt, NewIncoming,
};

/// An observer trigger that is fired when an [`Endpoint`] encounters an error.
#[derive(Debug, Error, Event)]
pub enum EndpointError {
    /// A connection entity has had its connection component(s) unexpectedly removed
    #[error("Entity {0} does not have connection components")]
    MalformedConnectionEntity(Entity),
    /// A connection entity has been unexpectedly despawned
    #[error("Entity {0} does not exist")]
    MissingConnectionEntity(Entity),
    /// The endpoint has been aborted due to an I/O error
    #[error(transparent)]
    IoError(std::io::Error),
}

/// Errors in the parameters being used to create a new connection
///
/// These arise before any I/O has been performed.
#[derive(Debug, Error)]
pub enum ConnectError {
    /// Attempted to create a new connection while the endpoint was not on an entity
    #[error("The endpoint is not on an entity")]
    EndpointNotOnEntity,
    /// An error occurred at the protocol level
    #[error(transparent)]
    Proto(#[from] quinn_proto::ConnectError),
}

/// A QUIC endpoint.
///
/// An endpoint corresponds to a single UDP socket, may host many connections,
/// and may act as both client and server for different connections.
///
/// # Usage
/// ```
/// # use bevy_ecs::system::{Query, assert_is_system};
/// # use bevy_quicsilver::Endpoint;
/// fn my_system(query: Query<&Endpoint>) {
///     for endpoint in query.iter() {
///         println!("{}", endpoint.open_connections());
///     }
/// }
/// # assert_is_system(my_system);
/// ```
#[derive(Debug)]
pub struct Endpoint {
    self_entity: Entity,
    endpoint: Arc<Mutex<quinn_proto::Endpoint>>,
    default_client_config: Option<ClientConfig>,
    connections: HashMap<ConnectionHandle, Entity>,
    socket: Arc<UdpSocket>,
    receiver: Receiver<Result<crate::socket::DatagramEvent, std::io::Error>>,
    ipv6: bool,
}

impl Component for Endpoint {
    const STORAGE_TYPE: StorageType = StorageType::Table;

    fn register_component_hooks(hooks: &mut ComponentHooks) {
        hooks
            .on_insert(|mut world, entity, _component_id| {
                // Record which entity we're on so we can pass it to connections
                let mut this = world.get_mut::<Self>(entity).unwrap();
                this.self_entity = entity;
            })
            .on_replace(|mut world, entity, _component_id| {
                // Record that we are no longer on an entity
                let mut this = world.get_mut::<Self>(entity).unwrap();
                this.self_entity = Entity::PLACEHOLDER;
            });
    }
}

impl Endpoint {
    /// Helper to construct an endpoint for use with outgoing connections, using the default [`EndpointConfig`].
    ///
    /// Note that `local_addr` is the *local* address to bind to, which should usually be a wildcard
    /// address like `0.0.0.0:0` or `[::]:0`, which allows communication with any reachable IPv4 or
    /// IPv6 address respectively from an OS-assigned port.
    ///
    /// Platform defaults for dual-stack sockets vary. For example, any socket bound to a wildcard
    /// IPv6 address on Windows will not by default be able to communicate with IPv4
    /// addresses. Portable applications should bind an address that matches the family they wish to
    /// communicate within.
    #[must_use = "Endpoints are components and do nothing if not spawned or inserted onto an entity"]
    pub fn new_client(
        local_addr: SocketAddr,
        default_client_config: Option<ClientConfig>,
    ) -> std::io::Result<Self> {
        std::net::UdpSocket::bind(local_addr).and_then(|socket| {
            Self::new(
                socket,
                EndpointConfig::default(),
                default_client_config,
                None,
                None,
            )
        })
    }

    /// Helper to construct an endpoint for use with incoming connections, using the default [`EndpointConfig`].
    ///
    /// Note that `local_addr` is the *local* address to bind to, which should usually be a wildcard
    /// address like `0.0.0.0` or `[::]` with a well-known, non-zero port number,
    /// which allows communication with any reachable IPv4 or IPv6 address respectively.
    ///
    /// If the specified port number is 0, the OS will assign a random port number,
    /// which you will need to somehow share with any clients that want to connect to the endpoint.
    ///
    /// Platform defaults for dual-stack sockets vary. For example, any socket bound to a wildcard
    /// IPv6 address on Windows will not by default be able to communicate with IPv4
    /// addresses. Portable applications should bind an address that matches the family they wish to
    /// communicate within.
    #[must_use = "Endpoints are components and do nothing if not spawned or inserted onto an entity"]
    pub fn new_server(
        local_addr: SocketAddr,
        server_config: ServerConfig,
    ) -> std::io::Result<Self> {
        std::net::UdpSocket::bind(local_addr).and_then(|socket| {
            Self::new(
                socket,
                EndpointConfig::default(),
                None,
                Some(server_config),
                None,
            )
        })
    }

    /// Helper to construct an endpoint for use with both incoming and outgoing connections, using the default [`EndpointConfig`].
    ///
    /// Note that `local_addr` is the *local* address to bind to, which should usually be a wildcard
    /// address like `0.0.0.0` or `[::]` with a well-known, non-zero port number,
    /// which allows communication with any reachable IPv4 or IPv6 address respectively.
    ///
    /// If the specified port number is 0, the OS will assign a random port number,
    /// which you will need to somehow share with any clients that want to connect to the endpoint.
    ///
    /// Platform defaults for dual-stack sockets vary. For example, any socket bound to a wildcard
    /// IPv6 address on Windows will not by default be able to communicate with IPv4
    /// addresses. Portable applications should bind an address that matches the family they wish to
    /// communicate within.
    #[must_use = "Endpoints are components and do nothing if not spawned or inserted onto an entity"]
    pub fn new_client_host(
        local_addr: SocketAddr,
        default_client_config: ClientConfig,
        server_config: ServerConfig,
    ) -> std::io::Result<Self> {
        std::net::UdpSocket::bind(local_addr).and_then(|socket| {
            Self::new(
                socket,
                EndpointConfig::default(),
                Some(default_client_config),
                Some(server_config),
                None,
            )
        })
    }

    /// Construct an endpoint with the specified socket and configurations.
    #[must_use = "Endpoints are components and do nothing if not spawned or inserted onto an entity"]
    pub fn new(
        socket: std::net::UdpSocket,
        config: EndpointConfig,
        default_client_config: Option<ClientConfig>,
        server_config: Option<ServerConfig>,
        rng_seed: Option<[u8; 32]>,
    ) -> std::io::Result<Self> {
        let ipv6 = socket.local_addr()?.is_ipv6();
        UdpSocket::new(socket).map(|socket| {
            let socket = Arc::new(socket);

            let max_udp_payload_size = config.get_max_udp_payload_size();

            let endpoint = Arc::new(Mutex::new(quinn_proto::Endpoint::new(
                Arc::new(config),
                server_config.map(Arc::new),
                !socket.may_fragment(),
                rng_seed,
            )));

            let (sender, receiver) = crossbeam_channel::unbounded();

            // Receiving data is done in a dedicated task to minimize latency between data arriving and being processed
            let driver = UdpSocketRecvDriver::new(
                socket.clone(),
                max_udp_payload_size,
                endpoint.clone(),
                sender,
            );

            IoTaskPool::get().spawn(driver).detach();

            Self {
                self_entity: Entity::PLACEHOLDER,
                endpoint,
                default_client_config,
                connections: HashMap::default(),
                socket,
                receiver,
                ipv6,
            }
        })
    }

    /// Initiate a connection with the remote endpoint identified by the specified address and server name,
    /// using the default client config.
    ///
    /// The exact value of the `server_name` parameter must be included in the `subject_alt_names` field of the server's certificate,
    /// as described by [`rcgen::generate_simple_self_signed`].
    #[must_use = "Connections are components and do nothing if not spawned or inserted onto an entity"]
    pub fn connect(
        &mut self,
        server_address: SocketAddr,
        server_name: &str,
    ) -> Result<Connecting, ConnectError> {
        self.default_client_config
            .clone()
            .ok_or(quinn_proto::ConnectError::NoDefaultClientConfig.into())
            .and_then(|client_config| self.connect_with(server_address, server_name, client_config))
    }

    /// Initiate a connection with the remote endpoint identified by the specified address and server name,
    /// using the specified client config.
    ///
    /// The exact value of the `server_name` parameter must be included in the `subject_alt_names` field of the server's certificate,
    /// as described by [`rcgen::generate_simple_self_signed`].
    #[must_use = "Connections are components and do nothing if not spawned or inserted onto an entity"]
    pub fn connect_with(
        &mut self,
        mut server_address: SocketAddr,
        server_name: &str,
        client_config: ClientConfig,
    ) -> Result<Connecting, ConnectError> {
        if self.self_entity == Entity::PLACEHOLDER {
            // If our recorded entity value is still the placeholder, we haven't been inserted onto an entity yet
            // (Or we were inserted onto and then removed from an entity)
            return Err(ConnectError::EndpointNotOnEntity);
        }

        let now = Instant::now();

        // Handle mismatched address families, not handling this can cause the connection to silently fail
        match server_address {
            SocketAddr::V4(addr) if self.ipv6 => {
                // Local socket bound to IPv6, convert target IPv4 address to mapped IPv6
                server_address = (addr.ip().to_ipv6_mapped(), addr.port()).into();
            }
            SocketAddr::V6(addr) if !self.ipv6 => {
                // Local socket bound to IPv4
                if let Some(v4) = addr.ip().to_ipv4_mapped() {
                    // Target address is mapped IPv6, convert it back to IPv4
                    server_address = (v4, addr.port()).into();
                } else {
                    // Target is a normal IPv6 address, can't be handled by an IPv4 stack
                    return Err(
                        quinn_proto::ConnectError::InvalidRemoteAddress(server_address).into(),
                    );
                }
            }
            _ => {} // Address family matches already, no problem
        };

        self.endpoint
            .lock()
            .unwrap()
            .connect(now, client_config, server_address, server_name)
            .map(|(handle, connection)| Connecting::new(self.self_entity, handle, connection))
            .map_err(Into::into)
    }

    pub(crate) fn accept(
        &mut self,
        incoming: quinn_proto::Incoming,
        server_config: Option<Arc<ServerConfig>>,
    ) -> Result<(ConnectionHandle, quinn_proto::Connection), ConnectionError> {
        let mut response_buffer = Vec::new();
        self.endpoint
            .lock()
            .unwrap()
            .accept(
                incoming,
                Instant::now(),
                &mut response_buffer,
                server_config,
            )
            .map_err(|AcceptError { cause, response }| {
                if let Some(response) = response {
                    self.send_response(&response, &response_buffer);
                }
                cause
            })
    }

    pub(crate) fn refuse(&mut self, incoming: quinn_proto::Incoming) {
        let mut response_buffer = Vec::new();
        let transmit = self
            .endpoint
            .lock()
            .unwrap()
            .refuse(incoming, &mut response_buffer);
        self.send_response(&transmit, &response_buffer);
    }

    pub(crate) fn retry(&mut self, incoming: quinn_proto::Incoming) -> Result<(), RetryError> {
        let mut response_buffer = Vec::new();
        self.endpoint
            .lock()
            .unwrap()
            .retry(incoming, &mut response_buffer)
            .map(|transmit| self.send_response(&transmit, &response_buffer))
    }

    pub(crate) fn ignore(&mut self, incoming: quinn_proto::Incoming) {
        self.endpoint.lock().unwrap().ignore(incoming);
    }

    /// Internal method for endpoint-generated data, which can safely ignore the Result
    /// See <https://github.com/quinn-rs/quinn/blob/0.11.1/quinn/src/endpoint.rs#L504>
    fn send_response(&self, transmit: &quinn_proto::Transmit, buffer: &[u8]) {
        let _ = self.send(transmit, buffer);
    }

    /// Send some data over the network.
    pub(crate) fn send(
        &self,
        transmit: &quinn_proto::Transmit,
        buffer: &[u8],
    ) -> std::io::Result<()> {
        self.socket.send(&udp_transmit(transmit, buffer))
    }

    pub(crate) fn handle_event(
        &mut self,
        connection: ConnectionHandle,
        event: EndpointEvent,
    ) -> Option<ConnectionEvent> {
        self.endpoint
            .lock()
            .unwrap()
            .handle_event(connection, event)
    }

    pub(crate) fn knows_connection(&self, connection: ConnectionHandle) -> bool {
        self.connections.contains_key(&connection)
    }

    pub(crate) fn connection_inserted(&mut self, connection: ConnectionHandle, entity: Entity) {
        self.connections.insert(connection, entity);
    }

    /// Set the default client configuration used by [`Self::connect()`].
    pub fn set_default_client_config(&mut self, config: ClientConfig) {
        self.default_client_config = Some(config);
    }

    /// Replace the server configuration, affecting new incoming connections only.
    pub fn set_server_config(&mut self, server_config: Option<ServerConfig>) {
        self.endpoint
            .lock()
            .unwrap()
            .set_server_config(server_config.map(Arc::new));
    }

    /// Switch to a new UDP socket.
    ///
    /// Allows the endpoint’s address to be updated live, affecting all active connections.
    /// Incoming connections and connections to servers unreachable from the new address will be lost.
    ///
    /// On error, the old UDP socket is retained.
    #[expect(dead_code)] // Make pub when implemented
    fn rebind(&mut self, _new_socket: std::net::UdpSocket) -> std::io::Result<()> {
        todo!()
    }

    /// Get the local `SocketAddr` the underlying socket is bound to.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Get the number of connections that are currently open.
    pub fn open_connections(&self) -> usize {
        self.connections.len()
    }

    pub(crate) fn max_gso_segments(&self) -> usize {
        self.socket.max_gso_segments()
    }
}

pub(crate) fn poll_endpoints(
    mut commands: Commands,
    mut endpoint_query: Query<(Entity, &mut Endpoint, Has<KeepAlive>)>,
    mut connection_query: Query<(Entity, ConnectionQuery)>,
) {
    for (endpoint_entity, mut ecs_endpoint, keepalive) in &mut endpoint_query {
        debug_assert_eq!(endpoint_entity, ecs_endpoint.self_entity);

        let Endpoint {
            self_entity: _,
            endpoint,
            default_client_config: _,
            connections,
            socket: _,
            receiver,
            ipv6: _,
        } = &mut *ecs_endpoint;

        let mut transmits = Vec::new();

        let mut endpoint = endpoint.lock().unwrap();
        for event in receiver.try_iter() {
            match event {
                Ok(event) => match event.event {
                    DatagramEvent::ConnectionEvent(handle, event) => {
                        let &connection_entity = connections
                            .get(&handle)
                            .expect("ConnectionHandle {handle:?} is missing Entity mapping");

                        match connection_query.get_mut(connection_entity) {
                            Ok((_, connection)) => {
                                let (_, connection) = connection.get();
                                if connection.handle == handle
                                    && connection.endpoint == endpoint_entity
                                {
                                    connection.handle_event(event);
                                } else {
                                    // A new connection was inserted onto the entity, overwriting the previous connection
                                    endpoint.handle_event(handle, EndpointEvent::drained());
                                    connections.remove(&handle);
                                }
                            }
                            Err(QueryEntityError::QueryDoesNotMatch(connection_entity, _)) => {
                                endpoint.handle_event(handle, EndpointEvent::drained());
                                commands.trigger_targets(
                                    EndpointError::MalformedConnectionEntity(connection_entity),
                                    endpoint_entity,
                                );
                            }
                            Err(QueryEntityError::NoSuchEntity(connection_entity)) => {
                                endpoint.handle_event(handle, EndpointEvent::drained());
                                commands.trigger_targets(
                                    EndpointError::MissingConnectionEntity(connection_entity),
                                    endpoint_entity,
                                );
                            }
                            Err(QueryEntityError::AliasedMutability(_)) => unreachable!(),
                        }
                    }
                    DatagramEvent::NewConnection(incoming) => {
                        commands
                            .spawn(Incoming::new(incoming, endpoint_entity))
                            .trigger(NewIncoming);
                    }
                    DatagramEvent::Response(transmit) => {
                        transmits.push((transmit, event.response_buffer));
                    }
                },
                Err(e) => {
                    commands.trigger_targets(EndpointError::IoError(e), endpoint_entity);
                    commands
                        .entity(endpoint_entity)
                        .remove_or_despawn::<Endpoint>(keepalive);
                }
            }
        }

        drop(endpoint);

        for (transmit, buffer) in transmits {
            ecs_endpoint.send_response(&transmit, &buffer);
        }
    }
}

#[inline]
fn udp_ecn(ecn: quinn_proto::EcnCodepoint) -> quinn_udp::EcnCodepoint {
    match ecn {
        quinn_proto::EcnCodepoint::Ect0 => quinn_udp::EcnCodepoint::Ect0,
        quinn_proto::EcnCodepoint::Ect1 => quinn_udp::EcnCodepoint::Ect1,
        quinn_proto::EcnCodepoint::Ce => quinn_udp::EcnCodepoint::Ce,
    }
}

#[inline]
fn udp_transmit<'a>(transmit: &quinn_proto::Transmit, buffer: &'a [u8]) -> quinn_udp::Transmit<'a> {
    quinn_udp::Transmit {
        destination: transmit.destination,
        ecn: transmit.ecn.map(udp_ecn),
        contents: buffer,
        segment_size: transmit.segment_size,
        src_ip: transmit.src_ip,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::Ipv6Addr,
        sync::{Arc, Once},
    };

    use bevy_app::App;
    use bevy_ecs::{
        entity::Entity,
        event::Event,
        observer::Trigger,
        query::With,
        system::{Local, Query},
    };

    use crate::{
        connection::{Connection, ConnectionEstablished, NewBidirectionalStream},
        incoming::NewIncoming,
        tests::*,
        Incoming, IncomingResponse, RecvError,
    };

    use super::{Endpoint, EndpointError};

    const CLIENT_TO_SERVER: &[u8] = b"client -> server";
    const SERVER_TO_CLIENT: &[u8] = b"server -> client";

    fn setup_client_and_server() -> (Endpoint, Endpoint) {
        let addr = (Ipv6Addr::LOCALHOST, 0).into();
        let (client, server) = generate_crypto();

        (
            Endpoint::new_client(addr, Some(client)).unwrap(),
            Endpoint::new_server(addr, server).unwrap(),
        )
    }

    #[test]
    fn malformed_connection_error() {
        let mut app = app_one_error::<EndpointError>();

        let connections = connection(&mut app);

        app.world_mut()
            .entity_mut(connections.server)
            .remove::<Connection>();

        wait_for_observer(
            &mut app,
            Some(connections.endpoint),
            move |trigger: Trigger<EndpointError>| {
                assert!(matches!(
                    trigger.event(),
                    &EndpointError::MalformedConnectionEntity(entity) if entity == connections.server
                ));
            },
            None,
            "EndpointError did not fire",
        );
    }

    #[test]
    fn missing_connection_error() {
        let mut app = app_one_error::<EndpointError>();

        let connections = connection(&mut app);

        app.world_mut().entity_mut(connections.server).despawn();

        wait_for_observer(
            &mut app,
            Some(connections.endpoint),
            move |trigger: Trigger<EndpointError>| {
                assert!(matches!(
                    trigger.event(),
                    &EndpointError::MissingConnectionEntity(entity) if entity == connections.server
                ));
            },
            None,
            "EndpointError did not fire",
        );
    }

    #[test]
    fn two_endpoints_same_world() {
        // Setup app and endpoints
        let mut app = app_no_errors();

        let (client, server) = setup_client_and_server();

        let client = app.world_mut().spawn(client).id();
        let server = app.world_mut().spawn(server).id();

        // Establish connections
        let server_endpoint = app.world().get::<Endpoint>(server).unwrap();
        let server_addr = server_endpoint.local_addr().unwrap();

        let mut client_endpoint = app.world_mut().get_mut::<Endpoint>(client).unwrap();
        let client_connection = client_endpoint.connect(server_addr, "localhost").unwrap();
        let client_connection = app.world_mut().spawn(client_connection).id();

        wait_for_observer(
            &mut app,
            None,
            |_: Trigger<NewIncoming>| {},
            None,
            "NewIncoming did not fire",
        );

        let incoming = app
            .world_mut()
            .query_filtered::<Entity, With<Incoming>>()
            .single_mut(app.world_mut());

        app.world_mut()
            .send_event(IncomingResponse::accept(incoming));

        wait_for_observer(
            &mut app,
            Some(incoming),
            |_: Trigger<ConnectionEstablished>| {},
            None,
            "ConnectionEstablished did not fire",
        );

        // Open stream and send data
        let server_connection = incoming;

        let mut client = app
            .world_mut()
            .get_mut::<Connection>(client_connection)
            .unwrap();

        let client_stream = client.open_bi().unwrap();
        let mut stream = client.send_stream(client_stream).unwrap();
        stream.write_all(CLIENT_TO_SERVER).unwrap();
        stream.finish().unwrap();

        wait_for_observer(
            &mut app,
            Some(server_connection),
            |_: Trigger<NewBidirectionalStream>| {},
            None,
            "NewBidirectionalStream did not fire",
        );

        // Send data the other way
        let mut server = app
            .world_mut()
            .get_mut::<Connection>(server_connection)
            .unwrap();
        let server_stream = server.accept_bi().unwrap();

        let mut stream = server.send_stream(server_stream).unwrap();
        stream.write_all(SERVER_TO_CLIENT).unwrap();
        stream.finish().unwrap();

        // Receive data
        let system = app.world_mut().register_system(
            move |mut query: Query<&mut Connection>, mut data: Local<Vec<u8>>| {
                let mut server = query.get_mut(server_connection).unwrap();
                let mut stream = server.recv_stream(server_stream).unwrap();
                match stream.read_chunk(usize::MAX, true) {
                    Ok(None) => {
                        assert_eq!(*data, CLIENT_TO_SERVER);
                        return false;
                    }
                    Ok(Some(chunk)) => data.extend_from_slice(&chunk.bytes),
                    Err(RecvError::Blocked) => {}
                    Err(e) => panic!("{}", e),
                }
                true
            },
        );

        while app.world_mut().run_system(system).unwrap() {
            app.update();
        }

        // Receive data the other way
        let system = app.world_mut().register_system(
            move |mut query: Query<&mut Connection>, mut data: Local<Vec<u8>>| {
                let mut client = query.get_mut(client_connection).unwrap();
                let mut stream = client.recv_stream(client_stream).unwrap();
                match stream.read_chunk(usize::MAX, true) {
                    Ok(None) => {
                        assert_eq!(*data, SERVER_TO_CLIENT);
                        return false;
                    }
                    Ok(Some(chunk)) => data.extend_from_slice(&chunk.bytes),
                    Err(RecvError::Blocked) => {}
                    Err(e) => panic!("{}", e),
                }
                true
            },
        );

        while app.world_mut().run_system(system).unwrap() {
            app.update();
        }
    }

    #[test]
    fn two_endpoints_different_worlds() {
        // Modified impl to handle this test having two worlds
        fn wait_for_observer<E: Event>(
            target: &mut App,
            other: &mut App,
            entity: Option<Entity>,
            otherwise: &str,
        ) {
            let has_run = Arc::new(Once::new());

            {
                let has_run = has_run.clone();
                if let Some(entity) = entity {
                    target
                        .world_mut()
                        .entity_mut(entity)
                        .observe(move |_trigger: Trigger<E>| {
                            has_run.call_once(|| {});
                        });
                } else {
                    target
                        .world_mut()
                        .add_observer(move |_trigger: Trigger<E>| {
                            has_run.call_once(|| {});
                        });
                }
            }

            for _ in 0..u16::MAX {
                target.update();
                other.update();
                if has_run.is_completed() {
                    return;
                }
            }

            panic!("{}", otherwise);
        }

        // Setup app and endpoints
        let mut client_app = app_no_errors();
        let mut server_app = app_no_errors();

        let (client, server) = setup_client_and_server();

        let client = client_app.world_mut().spawn(client).id();
        let server = server_app.world_mut().spawn(server).id();

        // Establish connections
        let server_endpoint = server_app.world().get::<Endpoint>(server).unwrap();
        let server_addr = server_endpoint.local_addr().unwrap();

        let mut client_endpoint = client_app.world_mut().get_mut::<Endpoint>(client).unwrap();
        let client_connection = client_endpoint.connect(server_addr, "localhost").unwrap();
        let client_connection = client_app.world_mut().spawn(client_connection).id();

        wait_for_observer::<NewIncoming>(
            &mut server_app,
            &mut client_app,
            None,
            "NewIncoming did not fire",
        );

        let incoming = server_app
            .world_mut()
            .query_filtered::<Entity, With<Incoming>>()
            .single_mut(server_app.world_mut());

        server_app
            .world_mut()
            .send_event(IncomingResponse::accept(incoming));

        wait_for_observer::<ConnectionEstablished>(
            &mut server_app,
            &mut client_app,
            Some(incoming),
            "ConnectionEstablished did not fire",
        );

        // Open stream and send data
        let server_connection = incoming;

        let mut client = client_app
            .world_mut()
            .get_mut::<Connection>(client_connection)
            .unwrap();

        let client_stream = client.open_bi().unwrap();
        let mut stream = client.send_stream(client_stream).unwrap();
        stream.write_all(CLIENT_TO_SERVER).unwrap();
        stream.finish().unwrap();

        wait_for_observer::<NewBidirectionalStream>(
            &mut server_app,
            &mut client_app,
            Some(server_connection),
            "NewBidirectionalStream did not fire",
        );

        // Send data the other way
        let mut server = server_app
            .world_mut()
            .get_mut::<Connection>(server_connection)
            .unwrap();
        let server_stream = server.accept_bi().unwrap();

        let mut stream = server.send_stream(server_stream).unwrap();
        stream.write_all(SERVER_TO_CLIENT).unwrap();
        stream.finish().unwrap();

        // Receive data
        let system = server_app.world_mut().register_system(
            move |mut query: Query<&mut Connection>, mut data: Local<Vec<u8>>| {
                let mut server = query.get_mut(server_connection).unwrap();
                let mut stream = server.recv_stream(server_stream).unwrap();
                match stream.read_chunk(usize::MAX, true) {
                    Ok(None) => {
                        assert_eq!(*data, CLIENT_TO_SERVER);
                        return false;
                    }
                    Ok(Some(chunk)) => data.extend_from_slice(&chunk.bytes),
                    Err(RecvError::Blocked) => {}
                    Err(e) => panic!("{}", e),
                }
                true
            },
        );

        while server_app.world_mut().run_system(system).unwrap() {
            server_app.update();
            client_app.update();
        }

        // Receive data the other way
        let system = client_app.world_mut().register_system(
            move |mut query: Query<&mut Connection>, mut data: Local<Vec<u8>>| {
                let mut client = query.get_mut(client_connection).unwrap();
                let mut stream = client.recv_stream(client_stream).unwrap();
                match stream.read_chunk(usize::MAX, true) {
                    Ok(None) => {
                        assert_eq!(*data, SERVER_TO_CLIENT);
                        return false;
                    }
                    Ok(Some(chunk)) => data.extend_from_slice(&chunk.bytes),
                    Err(RecvError::Blocked) => {}
                    Err(e) => panic!("{}", e),
                }
                true
            },
        );

        while client_app.world_mut().run_system(system).unwrap() {
            server_app.update();
            client_app.update();
        }
    }
}
