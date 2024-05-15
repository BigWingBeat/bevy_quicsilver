use std::{net::SocketAddr, sync::Arc, time::Instant};

use bevy_ecs::{
    bundle::Bundle,
    component::Component,
    entity::{Entity, EntityHash},
    event::EventWriter,
    query::{Added, QueryData},
    system::{Commands, Query},
};
use hashbrown::HashMap;
use quinn_proto::{
    AcceptError, ClientConfig, ConnectError, ConnectionEvent, ConnectionHandle, DatagramEvent,
    EndpointConfig, EndpointEvent, ServerConfig,
};

use crate::{connection::Connection, incoming::Incoming, socket::UdpSocket, EntityError, Error};

/// A bundle for adding an [`Endpoint`] to an entity
#[derive(Debug, Bundle)]
pub struct EndpointBundle(EndpointImpl);

impl EndpointBundle {
    /// Helper to construct an endpoint for use with outgoing connections, using the default [`EndpointConfig`]
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
    ) -> Result<Self, Error> {
        EndpointImpl::new_client(local_addr, default_client_config).map(Self)
    }

    /// Helper to construct an endpoint for use with incoming connections, using the default [`EndpointConfig`]
    ///
    /// Note that `local_addr` is the *local* address to bind to, which should usually be a wildcard
    /// address like `0.0.0.0:0` or `[::]:0`, which allows communication with any reachable IPv4 or
    /// IPv6 address respectively from an OS-assigned port.
    ///
    /// Platform defaults for dual-stack sockets vary. For example, any socket bound to a wildcard
    /// IPv6 address on Windows will not by default be able to communicate with IPv4
    /// addresses. Portable applications should bind an address that matches the family they wish to
    /// communicate within.
    pub fn new_server(local_addr: SocketAddr, server_config: ServerConfig) -> Result<Self, Error> {
        EndpointImpl::new_server(local_addr, server_config).map(Self)
    }

    /// Helper to construct an endpoint for use with both incoming and outgoing connections, using the default [`EndpointConfig`]
    ///
    /// Note that `local_addr` is the *local* address to bind to, which should usually be a wildcard
    /// address like `0.0.0.0:0` or `[::]:0`, which allows communication with any reachable IPv4 or
    /// IPv6 address respectively from an OS-assigned port.
    ///
    /// Platform defaults for dual-stack sockets vary. For example, any socket bound to a wildcard
    /// IPv6 address on Windows will not by default be able to communicate with IPv4
    /// addresses. Portable applications should bind an address that matches the family they wish to
    /// communicate within.
    pub fn new_client_host(
        local_addr: SocketAddr,
        default_client_config: ClientConfig,
        server_config: ServerConfig,
    ) -> Result<Self, Error> {
        EndpointImpl::new_client_host(local_addr, default_client_config, server_config).map(Self)
    }

    /// Construct an endpoint with the specified socket and configurations
    pub fn new(
        socket: std::net::UdpSocket,
        config: EndpointConfig,
        default_client_config: Option<ClientConfig>,
        server_config: Option<ServerConfig>,
        rng_seed: Option<[u8; 32]>,
    ) -> Result<Self, Error> {
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

/// A QUIC endpoint.
///
/// An endpoint corresponds to a single UDP socket, may host many connections,
/// and may act as both client and server for different connections.
///
/// # Usage
/// ```
/// fn my_system(query: Query<Endpoint>) {
///     for endpoint in query.iter() {
///         println!("{}", endpoint.open_connections());
///     }
/// }
/// ```
#[derive(QueryData)]
#[query_data(mutable)]
pub struct Endpoint {
    entity: Entity,
    endpoint: &'static mut EndpointImpl,
}

impl EndpointItem<'_> {
    /// Set the default client configuration used by [`Self::connect()`]
    pub fn set_default_client_config(&mut self, config: ClientConfig) {
        self.endpoint.set_default_client_config(config)
    }

    /// Replace the server configuration, affecting new incoming connections only
    pub fn set_server_config(&mut self, server_config: Option<ServerConfig>) {
        self.endpoint.set_server_config(server_config)
    }

    pub(crate) fn handle_event(
        &mut self,
        connection: ConnectionHandle,
        event: EndpointEvent,
    ) -> Option<ConnectionEvent> {
        self.endpoint.handle_event(connection, event)
    }

    /// Initiate a connection with the remote endpoint identified by the specified address and server name,
    /// using the default client config. The returned [`Connection`] component should be inserted onto an entity.
    ///
    /// The exact value of the `server_name` parameter must be included in the `subject_alt_names` field of the server's certificate,
    /// as described by [`config_with_gen_self_signed`].
    ///
    /// [config_with_gen_self_signed]: crate::crypto::server::config_with_gen_self_signed
    pub fn connect(
        &mut self,
        server_address: SocketAddr,
        server_name: &str,
    ) -> Result<Connection, Error> {
        self.endpoint
            .connect(self.entity, server_address, server_name)
    }

    /// Initiate a connection with the remote endpoint identified by the specified address and server name,
    /// using the specified client config. The returned [`Connection`] component should be inserted onto an entity.
    ///
    /// The exact value of the `server_name` parameter must be included in the `subject_alt_names` field of the server's certificate,
    /// as described by [`config_with_gen_self_signed`].
    ///
    /// [config_with_gen_self_signed]: crate::crypto::server::config_with_gen_self_signed
    pub fn connect_with(
        &mut self,
        server_address: SocketAddr,
        server_name: &str,
        client_config: ClientConfig,
    ) -> Result<Connection, Error> {
        self.endpoint
            .connect_with(self.entity, server_address, server_name, client_config)
    }

    pub(crate) fn accept(
        &mut self,
        incoming: quinn_proto::Incoming,
        server_config: Option<Arc<ServerConfig>>,
    ) -> Result<(ConnectionHandle, quinn_proto::Connection), Error> {
        self.endpoint.accept(incoming, server_config)
    }

    pub(crate) fn refuse(&mut self, incoming: quinn_proto::Incoming) {
        self.endpoint.refuse(incoming)
    }

    pub(crate) fn retry(&mut self, incoming: quinn_proto::Incoming) -> Result<(), Error> {
        self.endpoint.retry(incoming)
    }

    pub(crate) fn ignore(&mut self, incoming: quinn_proto::Incoming) {
        self.endpoint.ignore(incoming)
    }

    /// Switch to a new UDP socket
    ///
    /// Allows the endpoint’s address to be updated live, affecting all active connections.
    /// Incoming connections and connections to servers unreachable from the new address will be lost.
    ///
    /// On error, the old UDP socket is retained.
    pub fn rebind(&mut self, new_socket: std::net::UdpSocket) -> std::io::Result<()> {
        self.endpoint.rebind(new_socket)
    }

    /// Get the local `SocketAddr` the underlying socket is bound to
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    /// Get the number of connections that are currently open
    pub fn open_connections(&self) -> usize {
        self.endpoint.open_connections()
    }

    pub(crate) fn max_gso_segments(&self) -> usize {
        self.endpoint.max_gso_segments()
    }
}

impl EndpointReadOnlyItem<'_> {
    /// Get the local `SocketAddr` the underlying socket is bound to
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    /// Get the number of connections that are currently open
    pub fn open_connections(&self) -> usize {
        self.endpoint.open_connections()
    }

    pub(crate) fn max_gso_segments(&self) -> usize {
        self.endpoint.max_gso_segments()
    }
}

/// Actual underlying component type behind the [`EndpointBundle`] bundle and [`Endpoint`] querydata types
#[derive(Debug, Component)]
struct EndpointImpl {
    endpoint: quinn_proto::Endpoint,
    default_client_config: Option<ClientConfig>,
    connections: HashMap<ConnectionHandle, Entity, EntityHash>,
    socket: UdpSocket,
}

impl EndpointImpl {
    fn new_client(
        local_addr: SocketAddr,
        default_client_config: Option<ClientConfig>,
    ) -> Result<Self, Error> {
        std::net::UdpSocket::bind(local_addr)
            .map_err(Into::into)
            .and_then(|socket| {
                Self::new(
                    socket,
                    EndpointConfig::default(),
                    default_client_config,
                    None,
                    None,
                )
            })
    }

    fn new_server(local_addr: SocketAddr, server_config: ServerConfig) -> Result<Self, Error> {
        std::net::UdpSocket::bind(local_addr)
            .map_err(Into::into)
            .and_then(|socket| {
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
    ) -> Result<Self, Error> {
        std::net::UdpSocket::bind(local_addr)
            .map_err(Into::into)
            .and_then(|socket| {
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
    ) -> Result<Self, Error> {
        UdpSocket::new(socket, config.get_max_udp_payload_size())
            .map(|socket| Self {
                endpoint: quinn_proto::Endpoint::new(
                    Arc::new(config),
                    server_config.map(Arc::new),
                    !socket.may_fragment(),
                    rng_seed,
                ),
                default_client_config,
                connections: HashMap::default(),
                socket,
            })
            .map_err(Into::into)
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
    ) -> Result<Connection, Error> {
        self.default_client_config
            .clone()
            .ok_or(ConnectError::NoDefaultClientConfig.into())
            .and_then(|client_config| {
                self.connect_with(self_entity, server_address, server_name, client_config)
            })
    }

    fn connect_with(
        &mut self,
        self_entity: Entity,
        server_address: SocketAddr,
        server_name: &str,
        client_config: ClientConfig,
    ) -> Result<Connection, Error> {
        let now = Instant::now();
        // TODO: Why https://github.com/quinn-rs/quinn/blob/0.10.2/quinn/src/endpoint.rs#L185-L192
        self.endpoint
            .connect(now, client_config, server_address, server_name)
            .map_err(Into::into)
            .map(|(handle, connection)| Connection::new(self_entity, handle, connection))
    }

    fn accept(
        &mut self,
        incoming: quinn_proto::Incoming,
        server_config: Option<Arc<ServerConfig>>,
    ) -> Result<(ConnectionHandle, quinn_proto::Connection), Error> {
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
                    self.send(&response, &response_buffer);
                }
                cause.into()
            })
    }

    fn refuse(&mut self, incoming: quinn_proto::Incoming) {
        let mut response_buffer = Vec::new();
        let transmit = self.endpoint.refuse(incoming, &mut response_buffer);
        self.send(&transmit, &response_buffer);
    }

    fn retry(&mut self, incoming: quinn_proto::Incoming) -> Result<(), Error> {
        let mut response_buffer = Vec::new();
        self.endpoint
            .retry(incoming, &mut response_buffer)
            .map(|transmit| self.send(&transmit, &response_buffer))
            .map_err(Into::into)
    }

    fn ignore(&mut self, incoming: quinn_proto::Incoming) {
        self.endpoint.ignore(incoming)
    }

    fn send(&mut self, transmit: &quinn_proto::Transmit, buffer: &[u8]) {
        // This Result can be safely ignored, see https://github.com/quinn-rs/quinn/blob/0.11.1/quinn/src/endpoint.rs#L504
        let _ = self.socket.send(&udp_transmit(transmit, buffer));
    }

    fn set_default_client_config(&mut self, config: ClientConfig) {
        self.default_client_config = Some(config);
    }

    fn set_server_config(&mut self, server_config: Option<ServerConfig>) {
        self.endpoint.set_server_config(server_config.map(Arc::new))
    }

    fn rebind(&mut self, new_socket: std::net::UdpSocket) -> std::io::Result<()> {
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

pub(crate) fn find_new_connections(
    new_connections: Query<(Entity, &Connection), Added<Connection>>,
    mut endpoints: Query<Endpoint>,
) {
    for (entity, connection) in new_connections.iter() {
        let mut endpoint = endpoints
            .get_mut(connection.endpoint)
            .expect("Endpoint entity has gone missing");

        endpoint
            .endpoint
            .connections
            .insert(connection.handle, entity);
    }
}

pub(crate) fn poll_endpoints(
    mut commands: Commands,
    mut endpoint_query: Query<Endpoint>,
    mut connection_query: Query<&mut Connection>,
    mut error_events: EventWriter<EntityError>,
) {
    let now = Instant::now();
    for EndpointItem {
        entity,
        endpoint: mut endpoint_impl,
    } in endpoint_query.iter_mut()
    {
        let EndpointImpl {
            endpoint,
            default_client_config: _,
            connections,
            socket,
        } = &mut *endpoint_impl;

        let mut transmits = Vec::new();

        if let Err(e) = socket.receive(|meta, data| {
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
                    let mut connection = connections
                        .get(&handle)
                        .and_then(|&entity| connection_query.get_mut(entity).ok())
                        .expect("Got event for unknown connection");

                    connection.handle_event(event);
                }
                Some(DatagramEvent::NewConnection(incoming)) => {
                    commands.spawn(Incoming {
                        incoming,
                        endpoint_entity: entity,
                    });
                }
                Some(DatagramEvent::Response(transmit)) => {
                    transmits.push((transmit, response_buffer));
                }
                None => {}
            }
        }) {
            error_events.send(EntityError {
                entity,
                error: e.into(),
            });
        }

        for (transmit, buffer) in transmits {
            endpoint_impl.send(&transmit, &buffer);
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
