use std::{net::SocketAddr, sync::Arc, time::Instant};

use bevy_ecs::{
    bundle::Bundle,
    component::Component,
    entity::Entity,
    event::Event,
    query::{Has, QueryData, QueryEntityError},
    system::{Commands, Query},
};
use hashbrown::HashMap;
use quinn_proto::{
    AcceptError, ClientConfig, ConnectError, ConnectionError, ConnectionEvent, ConnectionHandle,
    DatagramEvent, EndpointConfig, EndpointEvent, RetryError, ServerConfig,
};
use thiserror::Error;

use crate::{
    connection::{ConnectingBundle, ConnectionImpl},
    incoming::Incoming,
    socket::UdpSocket,
    KeepAlive, KeepAliveEntityCommandsExt,
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

/// A bundle for adding an [`Endpoint`] to an entity.
#[derive(Debug, Bundle)]
#[must_use = "Endpoints are components and do nothing if not spawned or inserted onto an entity"]
pub struct EndpointBundle(EndpointImpl);

impl EndpointBundle {
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
    pub fn new_client(
        local_addr: SocketAddr,
        default_client_config: Option<ClientConfig>,
    ) -> std::io::Result<Self> {
        EndpointImpl::new_client(local_addr, default_client_config).map(Self)
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
    pub fn new_server(
        local_addr: SocketAddr,
        server_config: ServerConfig,
    ) -> std::io::Result<Self> {
        EndpointImpl::new_server(local_addr, server_config).map(Self)
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
    pub fn new_client_host(
        local_addr: SocketAddr,
        default_client_config: ClientConfig,
        server_config: ServerConfig,
    ) -> std::io::Result<Self> {
        EndpointImpl::new_client_host(local_addr, default_client_config, server_config).map(Self)
    }

    /// Construct an endpoint with the specified socket and configurations.
    pub fn new(
        socket: std::net::UdpSocket,
        config: EndpointConfig,
        default_client_config: Option<ClientConfig>,
        server_config: Option<ServerConfig>,
        rng_seed: Option<[u8; 32]>,
    ) -> std::io::Result<Self> {
        EndpointImpl::new(
            socket,
            config,
            default_client_config,
            server_config,
            rng_seed,
        )
        .map(Self)
    }
}

/// A query parameter for a QUIC endpoint.
/// For available methods when querying entities with this type, see [`EndpointItem`] and [`EndpointReadOnlyItem`].
///
/// An endpoint corresponds to a single UDP socket, may host many connections,
/// and may act as both client and server for different connections.
///
/// # Usage
/// ```
/// # use bevy_ecs::system::{Query, assert_is_system};
/// # use bevy_quicsilver::Endpoint;
/// fn my_system(query: Query<Endpoint>) {
///     for endpoint in query.iter() {
///         println!("{}", endpoint.open_connections());
///     }
/// }
/// # assert_is_system(my_system);
/// ```
#[derive(QueryData)]
#[query_data(mutable)]
pub struct Endpoint {
    entity: Entity,
    pub(crate) endpoint: &'static mut EndpointImpl,
}

impl EndpointItem<'_> {
    /// Set the default client configuration used by [`Self::connect()`].
    pub fn set_default_client_config(&mut self, config: ClientConfig) {
        self.endpoint.set_default_client_config(config);
    }

    /// Replace the server configuration, affecting new incoming connections only.
    pub fn set_server_config(&mut self, server_config: Option<ServerConfig>) {
        self.endpoint.set_server_config(server_config);
    }

    pub(crate) fn handle_event(
        &mut self,
        connection: ConnectionHandle,
        event: EndpointEvent,
    ) -> Option<ConnectionEvent> {
        self.endpoint.handle_event(connection, event)
    }

    /// Initiate a connection with the remote endpoint identified by the specified address and server name,
    /// using the default client config.
    ///
    /// The exact value of the `server_name` parameter must be included in the `subject_alt_names` field of the server's certificate,
    /// as described by [`rcgen::generate_simple_self_signed`].
    pub fn connect(
        &mut self,
        server_address: SocketAddr,
        server_name: &str,
    ) -> Result<ConnectingBundle, ConnectError> {
        self.endpoint
            .connect(self.entity, server_address, server_name)
    }

    /// Initiate a connection with the remote endpoint identified by the specified address and server name,
    /// using the specified client config.
    ///
    /// The exact value of the `server_name` parameter must be included in the `subject_alt_names` field of the server's certificate,
    /// as described by [`rcgen::generate_simple_self_signed`].
    pub fn connect_with(
        &mut self,
        server_address: SocketAddr,
        server_name: &str,
        client_config: ClientConfig,
    ) -> Result<ConnectingBundle, ConnectError> {
        self.endpoint
            .connect_with(self.entity, server_address, server_name, client_config)
    }

    pub(crate) fn accept(
        &mut self,
        incoming: quinn_proto::Incoming,
        server_config: Option<Arc<ServerConfig>>,
    ) -> Result<(ConnectionHandle, quinn_proto::Connection), ConnectionError> {
        self.endpoint.accept(incoming, server_config)
    }

    pub(crate) fn refuse(&mut self, incoming: quinn_proto::Incoming) {
        self.endpoint.refuse(incoming);
    }

    pub(crate) fn retry(&mut self, incoming: quinn_proto::Incoming) -> Result<(), RetryError> {
        self.endpoint.retry(incoming)
    }

    pub(crate) fn ignore(&mut self, incoming: quinn_proto::Incoming) {
        self.endpoint.ignore(incoming);
    }

    /// Switch to a new UDP socket.
    ///
    /// Allows the endpoint’s address to be updated live, affecting all active connections.
    /// Incoming connections and connections to servers unreachable from the new address will be lost.
    ///
    /// On error, the old UDP socket is retained.
    #[expect(dead_code)] // Make pub when implemented
    fn rebind(&mut self, new_socket: std::net::UdpSocket) -> std::io::Result<()> {
        self.endpoint.rebind(new_socket)
    }

    /// Get the local `SocketAddr` the underlying socket is bound to.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    /// Get the number of connections that are currently open.
    pub fn open_connections(&self) -> usize {
        self.endpoint.open_connections()
    }

    pub(crate) fn max_gso_segments(&self) -> usize {
        self.endpoint.max_gso_segments()
    }

    /// Send some data over the network.
    pub(crate) fn send(
        &self,
        transmit: &quinn_proto::Transmit,
        buffer: &[u8],
    ) -> std::io::Result<()> {
        self.endpoint.send(transmit, buffer)
    }
}

impl EndpointReadOnlyItem<'_> {
    /// Get the local `SocketAddr` the underlying socket is bound to.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    /// Get the number of connections that are currently open.
    pub fn open_connections(&self) -> usize {
        self.endpoint.open_connections()
    }

    #[expect(dead_code)]
    pub(crate) fn max_gso_segments(&self) -> usize {
        self.endpoint.max_gso_segments()
    }

    /// Send some data over the network.
    #[expect(dead_code)]
    pub(crate) fn send(
        &self,
        transmit: &quinn_proto::Transmit,
        buffer: &[u8],
    ) -> std::io::Result<()> {
        self.endpoint.send(transmit, buffer)
    }
}

/// Underlying component type behind the [`EndpointBundle`] bundle and [`Endpoint`] querydata types.
#[derive(Debug, Component)]
pub(crate) struct EndpointImpl {
    endpoint: quinn_proto::Endpoint,
    default_client_config: Option<ClientConfig>,
    pub(crate) connections: HashMap<ConnectionHandle, Entity>,
    socket: UdpSocket,
    ipv6: bool,
}

impl EndpointImpl {
    fn new_client(
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

    fn new_server(local_addr: SocketAddr, server_config: ServerConfig) -> std::io::Result<Self> {
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

    fn new_client_host(
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

    fn new(
        socket: std::net::UdpSocket,
        config: EndpointConfig,
        default_client_config: Option<ClientConfig>,
        server_config: Option<ServerConfig>,
        rng_seed: Option<[u8; 32]>,
    ) -> std::io::Result<Self> {
        let ipv6 = socket.local_addr()?.is_ipv6();
        UdpSocket::new(socket, config.get_max_udp_payload_size()).map(|socket| Self {
            endpoint: quinn_proto::Endpoint::new(
                Arc::new(config),
                server_config.map(Arc::new),
                !socket.may_fragment(),
                rng_seed,
            ),
            default_client_config,
            connections: HashMap::default(),
            socket,
            ipv6,
        })
    }

    pub(crate) fn handle_event(
        &mut self,
        connection: ConnectionHandle,
        event: EndpointEvent,
    ) -> Option<ConnectionEvent> {
        self.endpoint.handle_event(connection, event)
    }

    fn connect(
        &mut self,
        self_entity: Entity,
        server_address: SocketAddr,
        server_name: &str,
    ) -> Result<ConnectingBundle, ConnectError> {
        self.default_client_config
            .clone()
            .ok_or(ConnectError::NoDefaultClientConfig)
            .and_then(|client_config| {
                self.connect_with(self_entity, server_address, server_name, client_config)
            })
    }

    fn connect_with(
        &mut self,
        self_entity: Entity,
        mut server_address: SocketAddr,
        server_name: &str,
        client_config: ClientConfig,
    ) -> Result<ConnectingBundle, ConnectError> {
        let now = Instant::now();

        // Handle mismatched address families, not handling this can cause the connection to silently fail
        match server_address {
            SocketAddr::V4(addr) if self.ipv6 => {
                // Local socket bound to IPv6, convert target IPv4 address to mapped IPv6
                server_address = (addr.ip().to_ipv6_mapped(), addr.port()).into()
            }
            SocketAddr::V6(addr) if !self.ipv6 => {
                // Local socket bound to IPv4
                if let Some(v4) = addr.ip().to_ipv4_mapped() {
                    // Target address is mapped IPv6, convert it back to IPv4
                    server_address = (v4, addr.port()).into();
                } else {
                    // Target is a normal IPv6 address, can't be handled by an IPv4 stack
                    return Err(ConnectError::InvalidRemoteAddress(server_address));
                }
            }
            _ => {} // Address family matches already, no problem
        };

        self.endpoint
            .connect(now, client_config, server_address, server_name)
            .map(|(handle, connection)| {
                ConnectingBundle::new(ConnectionImpl::new(self_entity, handle, connection))
            })
    }

    fn accept(
        &mut self,
        incoming: quinn_proto::Incoming,
        server_config: Option<Arc<ServerConfig>>,
    ) -> Result<(ConnectionHandle, quinn_proto::Connection), ConnectionError> {
        let mut response_buffer = Vec::new();
        self.endpoint
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

    fn refuse(&mut self, incoming: quinn_proto::Incoming) {
        let mut response_buffer = Vec::new();
        let transmit = self.endpoint.refuse(incoming, &mut response_buffer);
        self.send_response(&transmit, &response_buffer);
    }

    fn retry(&mut self, incoming: quinn_proto::Incoming) -> Result<(), RetryError> {
        let mut response_buffer = Vec::new();
        self.endpoint
            .retry(incoming, &mut response_buffer)
            .map(|transmit| self.send_response(&transmit, &response_buffer))
    }

    fn ignore(&mut self, incoming: quinn_proto::Incoming) {
        self.endpoint.ignore(incoming);
    }

    /// Internal method for endpoint-generated data, which can safely ignore the Result
    /// See <https://github.com/quinn-rs/quinn/blob/0.11.1/quinn/src/endpoint.rs#L504>
    fn send_response(&self, transmit: &quinn_proto::Transmit, buffer: &[u8]) {
        let _ = self.send(transmit, buffer);
    }

    pub(crate) fn send(
        &self,
        transmit: &quinn_proto::Transmit,
        buffer: &[u8],
    ) -> std::io::Result<()> {
        self.socket.send(&udp_transmit(transmit, buffer))
    }

    fn set_default_client_config(&mut self, config: ClientConfig) {
        self.default_client_config = Some(config);
    }

    fn set_server_config(&mut self, server_config: Option<ServerConfig>) {
        self.endpoint.set_server_config(server_config.map(Arc::new));
    }

    fn rebind(&mut self, _new_socket: std::net::UdpSocket) -> std::io::Result<()> {
        todo!()
    }

    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    fn open_connections(&self) -> usize {
        self.connections.len()
    }

    pub(crate) fn max_gso_segments(&self) -> usize {
        self.socket.max_gso_segments()
    }
}

pub(crate) fn poll_endpoints(
    mut commands: Commands,
    mut endpoint_query: Query<(Endpoint, Has<KeepAlive>)>,
    mut connection_query: Query<(Entity, &mut ConnectionImpl)>,
) {
    // TODO: https://discord.com/channels/691052431525675048/747940465936040017/1284609471334580417
    // Have this receive loop run in a dedicated thread, for more accurate timing information.
    // Currently this is a normal system that runs once per frame, so there can be up to a whole frame of delay between
    // data arriving in the socket and this system seeing it, causing the `now` instant to be slightly inaccurate.
    // Having a dedicated thread would allow us to poll the socket way faster, or even use async to `await` on it.
    // This would make `now` much more accurate, resulting in a better RTT estimate, and higher quality replication.
    let now = Instant::now();
    for (
        EndpointItem {
            entity: endpoint_entity,
            endpoint: mut endpoint_impl,
        },
        keepalive,
    ) in &mut endpoint_query
    {
        let EndpointImpl {
            endpoint,
            default_client_config: _,
            connections,
            socket,
            ipv6: _,
        } = &mut *endpoint_impl;

        let mut transmits = Vec::new();

        if let Err(error) = socket.receive(|meta, data| {
            let mut response_buffer = Vec::new();
            match endpoint.handle(
                now,
                meta.addr,
                meta.dst_ip,
                meta.ecn.map(proto_ecn),
                data,
                &mut response_buffer,
            ) {
                Some(DatagramEvent::ConnectionEvent(handle, event)) => {
                    let &connection_entity = connections
                        .get(&handle)
                        .expect("ConnectionHandle {handle:?} is missing Entity mapping");

                    match connection_query.get_mut(connection_entity) {
                        Ok((_, mut connection)) => {
                            if connection.handle == handle && connection.endpoint == endpoint_entity
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
                Some(DatagramEvent::NewConnection(incoming)) => {
                    commands.spawn(Incoming {
                        incoming,
                        endpoint_entity,
                    });
                }
                Some(DatagramEvent::Response(transmit)) => {
                    transmits.push((transmit, response_buffer));
                }
                None => {}
            }
        }) {
            commands.trigger_targets(EndpointError::IoError(error), endpoint_entity);
            commands
                .entity(endpoint_entity)
                .remove_or_despawn::<EndpointImpl>(keepalive);
        }

        for (transmit, buffer) in transmits {
            endpoint_impl.send_response(&transmit, &buffer);
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
fn proto_ecn(ecn: quinn_udp::EcnCodepoint) -> quinn_proto::EcnCodepoint {
    match ecn {
        quinn_udp::EcnCodepoint::Ect0 => quinn_proto::EcnCodepoint::Ect0,
        quinn_udp::EcnCodepoint::Ect1 => quinn_proto::EcnCodepoint::Ect1,
        quinn_udp::EcnCodepoint::Ce => quinn_proto::EcnCodepoint::Ce,
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
    use std::net::Ipv6Addr;

    use bevy_app::App;
    use bevy_ecs::{
        entity::Entity,
        event::Events,
        observer::Trigger,
        query::Without,
        system::{Query, ResMut, Resource},
    };
    use bytes::Bytes;

    use crate::{
        connection::{Connecting, Connection, ConnectionEstablished, ConnectionImpl},
        incoming::NewIncoming,
        tests::*,
        Incoming, IncomingResponse,
    };

    use super::{Endpoint, EndpointBundle, EndpointError};

    #[derive(Debug, Resource, Default)]
    struct ConnectionEstablishedEntities(Vec<Entity>);

    #[derive(Debug, Resource, Default)]
    struct NewIncomingEntities(Vec<Entity>);

    #[test]
    fn malformed_connection_error() {
        let mut app = app_one_error::<EndpointError>();
        app.init_resource::<HasObserverTriggered>();

        let connections = connection(&mut app);

        app.world_mut()
            .entity_mut(connections.server)
            .remove::<ConnectionImpl>();

        app.world_mut()
            .query::<Connection>()
            .get_mut(app.world_mut(), connections.client)
            .unwrap()
            .send_datagram(Bytes::new())
            .unwrap();

        app.world_mut().entity_mut(connections.endpoint).observe(
            move |trigger: Trigger<EndpointError>,
                  mut endpoint: Query<Endpoint>,
                  mut res: ResMut<HasObserverTriggered>| {
                let _ = endpoint.get_mut(trigger.entity()).unwrap();
                assert!(matches!(
                    trigger.event(),
                    &EndpointError::MalformedConnectionEntity(e) if e == connections.server
                ));
                res.0 = true;
            },
        );

        app.update();
        app.update();
    }

    #[test]
    fn missing_connection_error() {
        let mut app = app_one_error::<EndpointError>();
        app.init_resource::<HasObserverTriggered>();

        let connections = connection(&mut app);

        app.world_mut().entity_mut(connections.server).despawn();

        app.world_mut()
            .query::<Connection>()
            .get_mut(app.world_mut(), connections.client)
            .unwrap()
            .send_datagram(Bytes::new())
            .unwrap();

        app.world_mut().entity_mut(connections.endpoint).observe(
            move |trigger: Trigger<EndpointError>,
                  mut endpoint: Query<Endpoint>,
                  mut res: ResMut<HasObserverTriggered>| {
                let _ = endpoint.get_mut(trigger.entity()).unwrap();
                assert!(matches!(
                    trigger.event(),
                    &EndpointError::MissingConnectionEntity(e) if e == connections.server
                ));
                res.0 = true;
            },
        );

        app.update();
        app.update();
    }

    fn setup_app() -> App {
        let mut app = app_no_errors();
        app.init_resource::<NewIncomingEntities>()
            .init_resource::<ConnectionEstablishedEntities>()
            .add_observer(
                |trigger: Trigger<NewIncoming>, mut entities: ResMut<NewIncomingEntities>| {
                    entities.0.push(trigger.entity());
                },
            )
            .add_observer(
                |trigger: Trigger<ConnectionEstablished>,
                 mut entities: ResMut<ConnectionEstablishedEntities>| {
                    entities.0.push(trigger.entity())
                },
            );
        app
    }

    macro_rules! establish_connection {
        ($client_app:ident, $server_app:ident, $client_endpoint_entity:ident, $server_endpoint_entity:ident) => {{
            // Get the `SocketAddr` that the client should connect to
            let server_addr = $server_app
                .world_mut()
                .query::<Endpoint>()
                .get($server_app.world(), $server_endpoint_entity)
                .unwrap()
                .local_addr()
                .unwrap();

            // Initiate connection from client to server
            let client_connection = $client_app
                .world_mut()
                .query::<Endpoint>()
                .get_mut($client_app.world_mut(), $client_endpoint_entity)
                .unwrap()
                .connect(server_addr, "localhost")
                .unwrap();

            // Spawn client-side connection
            let client_connection = $client_app.world_mut().spawn(client_connection).id();

            // Client connection sends packet to server
            $client_app.update();

            let stats = $client_app
                .world_mut()
                .query::<Connecting>()
                .get($client_app.world(), client_connection)
                .unwrap()
                .stats();

            assert_eq!(stats.udp_tx.datagrams, 1);
            assert_eq!(stats.frame_tx.crypto, 1);
            assert_eq!(stats.path.sent_packets, 1);

            // Server reads packet from client and spawns an Incoming
            $server_app.update();

            let events = $server_app.world().resource::<NewIncomingEntities>();
            let [server_connection] = events.0[..] else {
                panic!("NewIncoming should fire once but didn't: {:?}", events);
            };

            let incoming = $server_app
                .world_mut()
                .query::<&Incoming>()
                .get($server_app.world(), server_connection)
                .unwrap();

            assert_eq!(incoming.endpoint(), $server_endpoint_entity);

            $server_app
                .world_mut()
                .resource_mut::<Events<IncomingResponse>>()
                .send(IncomingResponse::accept(server_connection));

            // Incoming is replaced with server-side connection, server sends packets to client
            $server_app.update();

            let stats = $server_app
                .world_mut()
                .query_filtered::<Connecting, Without<Incoming>>()
                .get($server_app.world(), server_connection)
                .unwrap()
                .stats();

            assert_eq!(stats.frame_rx.crypto, 1);

            // Wait for connection to become fully established
            // TODO: for `two_endpoints_different_worlds`, why are so many updates needed
            $client_app.update();
            $server_app.update();
            $client_app.update();
            $server_app.update();
            $client_app.update();
            $server_app.update();

            (client_connection, server_connection)
        }};
    }

    macro_rules! test_datagrams {
        ($client_app:ident, $server_app:ident, $client_connection_entity:ident, $server_connection_entity:ident) => {{
            // Confirm that the connections can send data to each other
            let mut client_connection = $client_app
                .world_mut()
                .query::<Connection>()
                .get_mut($client_app.world_mut(), $client_connection_entity)
                .expect("0");

            client_connection
                .send_datagram("datagram client -> server".into())
                .expect("1");

            let mut server_connection = $server_app
                .world_mut()
                .query::<Connection>()
                .get_mut($server_app.world_mut(), $server_connection_entity)
                .expect("2");

            server_connection
                .send_datagram("datagram server -> client".into())
                .expect("3");

            // Transmit buffered datagrams
            $client_app.update();
            $server_app.update();
            $client_app.update();

            let mut client_connection = $client_app
                .world_mut()
                .query::<Connection>()
                .get_mut($client_app.world_mut(), $client_connection_entity)
                .expect("4");

            let datagram =
                String::from_utf8(client_connection.read_datagram().expect("5").to_vec())
                    .expect("6");
            assert_eq!(datagram, "datagram server -> client");

            let mut server_connection = $server_app
                .world_mut()
                .query::<Connection>()
                .get_mut($server_app.world_mut(), $server_connection_entity)
                .expect("7");

            let datagram =
                String::from_utf8(server_connection.read_datagram().expect("8").to_vec())
                    .expect("9");
            assert_eq!(datagram, "datagram client -> server");
        }};
    }

    #[test]
    fn one_endpoint() {
        let mut app = setup_app();
        let endpoint = endpoint();
        let endpoint_entity = app.world_mut().spawn(endpoint).id();

        let (client_connection, server_connection) =
            establish_connection!(app, app, endpoint_entity, endpoint_entity);

        let connection_established_entities =
            &app.world().resource::<ConnectionEstablishedEntities>().0;

        // One of these is for the client and the other is for the server, but we don't know which way round they are
        assert!(
            connection_established_entities == &[client_connection, server_connection]
                || connection_established_entities == &[server_connection, client_connection]
        );

        test_datagrams!(app, app, client_connection, server_connection);
    }

    #[test]
    fn two_endpoints_same_world() {
        let mut app = setup_app();

        let addr = (Ipv6Addr::LOCALHOST, 0).into();

        let (client, server) = generate_crypto();
        let client = EndpointBundle::new_client(addr, Some(client)).unwrap();
        let server = EndpointBundle::new_server(addr, server).unwrap();

        let client_endpoint = app.world_mut().spawn(client).id();
        let server_endpoint = app.world_mut().spawn(server).id();

        let (client_connection, server_connection) =
            establish_connection!(app, app, client_endpoint, server_endpoint);

        let connection_established_entities =
            &app.world().resource::<ConnectionEstablishedEntities>().0;

        // One of these is for the client and the other is for the server, but we don't know which way round they are
        assert!(
            connection_established_entities == &[client_connection, server_connection]
                || connection_established_entities == &[server_connection, client_connection]
        );

        test_datagrams!(app, app, client_connection, server_connection);
    }

    #[test]
    fn two_endpoints_different_worlds() {
        let mut client_app = setup_app();
        let mut server_app = setup_app();

        let addr = (Ipv6Addr::LOCALHOST, 0).into();

        let (client, server) = generate_crypto();
        let client = EndpointBundle::new_client(addr, Some(client)).unwrap();
        let server = EndpointBundle::new_server(addr, server).unwrap();

        let client_endpoint = client_app.world_mut().spawn(client).id();
        let server_endpoint = server_app.world_mut().spawn(server).id();

        let (client_connection, server_connection) =
            establish_connection!(client_app, server_app, client_endpoint, server_endpoint);

        // Each app should have 1 connection established event
        let established = &client_app
            .world()
            .resource::<ConnectionEstablishedEntities>()
            .0;
        assert_eq!(established, &[client_connection]);

        let established = &server_app
            .world()
            .resource::<ConnectionEstablishedEntities>()
            .0;
        assert_eq!(established, &[server_connection]);

        test_datagrams!(client_app, server_app, client_connection, server_connection);
    }
}
