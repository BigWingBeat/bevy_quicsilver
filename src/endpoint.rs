use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Instant,
};

use bevy_ecs::{
    component::Component,
    entity::{Entity, EntityHash},
    event::{EventReader, EventWriter},
    system::{Commands, EntityCommand, EntityCommands, In, Query},
    world::error,
};
use bytes::BytesMut;
use hashbrown::HashMap;
use quinn_proto::{
    ClientConfig, ConnectError, ConnectionHandle, DatagramEvent, EndpointConfig, ServerConfig,
    Transmit,
};
use quinn_udp::UdpSocketState;

use crate::{
    connection::{Connection, EndpointEvent},
    socket::UdpSocket,
    EntityError, Error,
};

/// The main entrypoint to the library. Wraps a [`quinn_proto::Endpoint`].
///
/// Unlike `quinn_proto`, this library *does* do actual I/O.
#[derive(Debug, Component)]
pub struct Endpoint {
    // TODO: Remove mutex when sync fix is released
    endpoint: Mutex<quinn_proto::Endpoint>,
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
                )
            })
    }

    /// Helper to construct an endpoint for use with incoming connections, using the default [`EndpointConfig`]
    ///
    /// Platform defaults for dual-stack sockets vary. For example, any socket bound to a wildcard
    /// IPv6 address on Windows will not by default be able to communicate with IPv4
    /// addresses. Portable applications should bind an address that matches the family they wish to
    /// communicate within.
    pub fn new_server(local_addr: SocketAddr, server_config: ServerConfig) -> Result<Self, Error> {
        std::net::UdpSocket::bind(local_addr)
            .map_err(Into::into)
            .and_then(|socket| {
                Self::new(socket, EndpointConfig::default(), None, Some(server_config))
            })
    }

    /// Helper to construct an endpoint for use with both incoming and outgoing connections, using the default [`EndpointConfig`]
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
                )
            })
    }

    /// Construct an endpoint with the specified socket and configurations
    pub fn new(
        socket: std::net::UdpSocket,
        config: EndpointConfig,
        default_client_config: Option<ClientConfig>,
        server_config: Option<ServerConfig>,
    ) -> Result<Self, Error> {
        UdpSocket::new(socket, config.get_max_udp_payload_size())
            .map(|socket| Self {
                endpoint: Mutex::new(quinn_proto::Endpoint::new(
                    Arc::new(config),
                    server_config.map(Arc::new),
                    !socket.may_fragment(),
                    None,
                )),
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
        self.endpoint
            .get_mut()
            .unwrap()
            .set_server_config(server_config.map(Arc::new))
    }

    fn connect(
        &mut self,
        self_entity: Entity,
        now: Instant,
        config: Option<ClientConfig>,
        server_addr: SocketAddr,
        server_name: &str,
        connection_entity: &mut EntityCommands,
    ) -> Result<(), ConnectError> {
        // TODO: Why https://github.com/quinn-rs/quinn/blob/0.10.2/quinn/src/endpoint.rs#L185-L192
        config
            .or(self.client_config.clone())
            .ok_or(ConnectError::NoDefaultClientConfig)
            .and_then(|config| {
                self.endpoint
                    .get_mut()
                    .unwrap()
                    .connect(now, config, server_addr, server_name)
            })
            .map(|(handle, connection)| {
                self.connections
                    .try_insert(handle, connection_entity.id())
                    .expect("Got duplicate connection handle");
                connection_entity.insert(Connection::new(self_entity, handle, connection));
            })
    }

    pub(crate) fn max_gso_segments(&self) -> usize {
        self.socket.max_gso_segments()
    }

    // /// Access the configuration used by this endpoint
    // pub fn config(&self) -> &EndpointConfig {
    //     self.endpoint.lock().unwrap().config()
    // }
}

/// Initiate a connection to the server identified by the given [`SocketAddr`] and `server_name`, using the default config.
/// The connection is added to the entity associated with the specified `EntityCommands`.
///
/// The exact value of the `server_name` parameter must be included in the `subject_alt_names` field of the server's certificate,
/// as described by [`config_with_gen_self_signed`]
pub fn connect(
    In((endpoint_entity, now, server_addr, server_name, connection_entity)): In<(
        Entity,
        Instant,
        SocketAddr,
        &str,
        &mut EntityCommands,
    )>,
    mut query: Query<&mut Endpoint>,
) -> Result<(), Error> {
    query
        .get_mut(endpoint_entity)
        .map_err(Into::into)
        .and_then(|mut endpoint| {
            endpoint
                .connect(
                    endpoint_entity,
                    now,
                    None,
                    server_addr,
                    server_name,
                    connection_entity,
                )
                .map_err(Into::into)
        })
}

/// Initiate a connection to the server identified by the given [`SocketAddr`] and `server_name`, using the specified config.
/// The connection is added to the entity associated with the specified `EntityCommands`.
///
/// The exact value of the `server_name` parameter must be included in the `subject_alt_names` field of the server's certificate,
/// as described by [`config_with_gen_self_signed`]
pub fn connect_with(
    In((endpoint_entity, now, config, server_addr, server_name, connection_entity)): In<(
        Entity,
        Instant,
        ClientConfig,
        SocketAddr,
        &str,
        &mut EntityCommands,
    )>,
    mut query: Query<&mut Endpoint>,
) -> Result<(), Error> {
    query
        .get_mut(endpoint_entity)
        .map_err(Into::into)
        .and_then(|mut endpoint| {
            endpoint
                .connect(
                    endpoint_entity,
                    now,
                    Some(config),
                    server_addr,
                    server_name,
                    connection_entity,
                )
                .map_err(Into::into)
        })
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
            match endpoint.get_mut().unwrap().handle(
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
                    if let Err(e) = endpoint
                        .get_mut()
                        .unwrap()
                        .accept(incoming, now, &mut Vec::new(), None)
                        .map(|(handle, connection)| {
                            let connection_entity = commands
                                .spawn(Connection::new(entity, handle, connection))
                                .id();

                            connections
                                .try_insert(handle, connection_entity)
                                .expect("Got duplicate connection handle");
                        })
                    {
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
