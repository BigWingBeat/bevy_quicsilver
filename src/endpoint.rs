use std::{net::SocketAddr, sync::Arc, time::Instant};

use bevy_ecs::{
    component::Component,
    entity::{Entity, EntityHash},
    event::{Event, EventReader, EventWriter},
    query::{Has, QueryEntityError},
    system::{Commands, Query},
};
use hashbrown::HashMap;
use quinn_proto::{
    ClientConfig, ConnectError, ConnectionHandle, DatagramEvent, EndpointConfig, ServerConfig,
};

use crate::{
    connection::{Connection, EndpointEvent},
    socket::UdpSocket,
    EntityError, Error, ErrorKind,
};

/// An event that initiates a connection to a remote endpoint
#[derive(Debug, Event)]
pub struct ConnectEvent {
    endpoint_entity: Entity,
    server_address: SocketAddr,
    server_name: String,
    connection_entity: Option<Entity>,
    client_config: Option<ClientConfig>,
}

impl ConnectEvent {
    /// Create a new event to initiate a connection with the specified endpoint,
    /// to the peer identified by the specified address and server name.
    /// If the connection succeeds, a new entity will be spawned with a [`Connection`] component.
    ///
    /// The exact value of the `server_name` parameter must be included in the `subject_alt_names` field of the server's certificate,
    /// as described by [`config_with_gen_self_signed`].
    ///
    /// [config_with_gen_self_signed]: crate::crypto::server::config_with_gen_self_signed
    pub fn new(endpoint_entity: Entity, server_address: SocketAddr, server_name: String) -> Self {
        Self {
            endpoint_entity,
            server_address,
            server_name,
            connection_entity: None,
            client_config: None,
        }
    }

    /// Create a new event to initiate a connection with the specified endpoint,
    /// to the peer identified by the specified address and server name.
    /// If the connection succeeds, a [`Connection`] component will be added to the given `connection_entity`.
    ///
    /// The exact value of the `server_name` parameter must be included in the `subject_alt_names` field of the server's certificate,
    /// as described by [`config_with_gen_self_signed`].
    ///
    /// [config_with_gen_self_signed]: crate::crypto::server::config_with_gen_self_signed
    pub fn new_with_entity(
        endpoint_entity: Entity,
        server_address: SocketAddr,
        server_name: String,
        connection_entity: Entity,
    ) -> Self {
        Self {
            endpoint_entity,
            server_address,
            server_name,
            connection_entity: Some(connection_entity),
            client_config: None,
        }
    }

    /// Overrides the endpoint's default client config just for this connection
    pub fn with_client_config(self, config: ClientConfig) -> Self {
        Self {
            client_config: Some(config),
            ..self
        }
    }
}

/// The main entrypoint to the library. Wraps a [`quinn_proto::Endpoint`].
///
/// Unlike `quinn_proto`, this library *does* do actual I/O.
#[derive(Debug, Component)]
pub struct Endpoint {
    endpoint: quinn_proto::Endpoint,
    client_config: Option<ClientConfig>,
    connections: HashMap<ConnectionHandle, Entity, EntityHash>,
    socket: UdpSocket,
}

impl Endpoint {
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

    /// Construct an endpoint with the specified socket and configurations
    pub fn new(
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
                client_config: default_client_config,
                connections: HashMap::default(),
                socket,
            })
            .map_err(Into::into)
    }

    /// Set the default client configuration used by [`Self::connect()`]
    pub fn set_default_client_config(&mut self, config: ClientConfig) {
        self.client_config = Some(config);
    }

    /// Replace the server configuration, affecting new incoming connections only
    pub fn set_server_config(&mut self, server_config: Option<ServerConfig>) {
        self.endpoint.set_server_config(server_config.map(Arc::new))
    }

    /// Switch to a new UDP socket
    ///
    /// Allows the endpoint’s address to be updated live, affecting all active connections.
    /// Incoming connections and connections to servers unreachable from the new address will be lost.
    ///
    /// On error, the old UDP socket is retained.
    pub fn rebind(&mut self, new_socket: std::net::UdpSocket) -> std::io::Result<()> {
        todo!()
    }

    /// Get the local `SocketAddr` the underlying socket is bound to
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Get the number of connections that are currently open
    pub fn open_connections(&self) -> usize {
        self.connections.len()
    }

    pub(crate) fn max_gso_segments(&self) -> usize {
        self.socket.max_gso_segments()
    }
}

fn poll_connect_events(
    mut commands: Commands,
    mut events: EventReader<ConnectEvent>,
    mut endpoints: Query<&mut Endpoint>,
    has_connection: Query<Has<Connection>>,
    mut error_events: EventWriter<EntityError>,
) {
    let now = Instant::now();
    for event in events.read() {
        if let Err(e) = endpoints
            .get_mut(event.endpoint_entity)
            .map_err(|e| {
                if let QueryEntityError::QueryDoesNotMatch(entity) = e {
                    ErrorKind::NoSuchEndpoint(entity, e).into()
                } else {
                    e.into()
                }
            })
            .and_then(|mut endpoint| {
                let connection_entity = event
                    .connection_entity
                    .map(|entity| {
                        has_connection
                            .get(entity)
                            .map_err(ErrorKind::QueryEntity)
                            .and_then(|has_connection| {
                                (!has_connection)
                                    .then_some(entity)
                                    .ok_or(ErrorKind::ConnectionAlreadyExists(entity))
                            })
                    })
                    .transpose()?;

                // TODO: Why https://github.com/quinn-rs/quinn/blob/0.10.2/quinn/src/endpoint.rs#L185-L192
                event
                    .client_config
                    .as_ref()
                    .or(endpoint.client_config.as_ref())
                    .ok_or(ConnectError::NoDefaultClientConfig.into())
                    .cloned()
                    .and_then(|config| {
                        endpoint
                            .endpoint
                            .connect(
                                now,
                                config.clone(),
                                event.server_address,
                                &event.server_name,
                            )
                            .map_err(Into::into)
                    })
                    .map(|(handle, connection)| {
                        let connection = Connection::new(event.endpoint_entity, handle, connection);

                        let connection_entity = if let Some(entity) = connection_entity {
                            commands.entity(entity).insert(connection).id()
                        } else {
                            commands.spawn(connection).id()
                        };

                        endpoint
                            .connections
                            .try_insert(handle, connection_entity)
                            .expect("Got duplicate connection handle");
                    })
            })
        {
            error_events.send(EntityError {
                entity: event.endpoint_entity,
                error: e,
            });
        }
    }
}

fn poll_endpoints(
    mut commands: Commands,
    mut endpoint_query: Query<(Entity, &mut Endpoint)>,
    mut connection_query: Query<&mut Connection>,
    inbound_events: EventReader<EndpointEvent>,
    mut error_events: EventWriter<EntityError>,
) {
    let now = Instant::now();
    for (entity, mut endpoint) in endpoint_query.iter_mut() {
        let Endpoint {
            endpoint,
            client_config,
            connections,
            socket,
        } = &mut *endpoint;

        // #1: .handle()

        if let Err(e) = socket.receive(|meta, data| {
            match endpoint.handle(
                now,
                meta.addr,
                meta.dst_ip,
                meta.ecn.map(proto_ecn),
                data,
                &mut Vec::new(),
            ) {
                Some(DatagramEvent::ConnectionEvent(handle, event)) => {
                    let mut connection = connections
                        .get(&handle)
                        .and_then(|&entity| connection_query.get_mut(entity).ok())
                        .expect("Got event for unknown connection");

                    connection.handle_event(event);
                }
                Some(DatagramEvent::NewConnection(incoming)) => {
                    if let Err(e) = endpoint.accept(incoming, now, &mut Vec::new(), None).map(
                        |(handle, connection)| {
                            let connection_entity = commands
                                .spawn(Connection::new(entity, handle, connection))
                                .id();

                            connections
                                .try_insert(handle, connection_entity)
                                .expect("Got duplicate connection handle");
                        },
                    ) {
                        if let Some(response) = e.response {
                            todo!()
                        }
                        error_events.send(EntityError {
                            entity,
                            error: e.cause.into(),
                        });
                    }
                }
                Some(DatagramEvent::Response(transmit)) => {
                    todo!()
                }
                None => {}
            }
        }) {
            error_events.send(EntityError {
                entity,
                error: e.into(),
            });
        }

        // #2: .handle_event()
        // #3: .poll_transmit()
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
