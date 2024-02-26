use std::net::SocketAddr;

use bevy_ecs::{
    component::Component,
    entity::{Entity, EntityHash},
    system::EntityCommands,
};
use hashbrown::HashMap;
use quinn_proto::{ClientConfig, ConnectError, ConnectionHandle, EndpointConfig, ServerConfig};
use quinn_udp::UdpState;

use crate::connection::Connection;

#[derive(Debug)]
pub(crate) struct ConnectionEvent {
    connection: ConnectionHandle,
    event: quinn_proto::ConnectionEvent,
}

/// The main entrypoint to the library. Wraps a [`quinn_proto::Endpoint`].
///
/// Unlike `quinn_proto`, this library *does* do actual I/O.
#[derive(Debug, Component)]
pub struct Endpoint {
    endpoint: quinn_proto::Endpoint,
    client_config: Option<ClientConfig>,
    connections: HashMap<ConnectionHandle, Entity, EntityHash>,
    udp_state: UdpState,
}

impl Endpoint {
    /// Create an endpoint for outgoing connections, with the specified endpoint config & default client config.
    pub fn new_client(
        endpoint_config: EndpointConfig,
        client_config: Option<ClientConfig>,
    ) -> Self {
        let endpoint = quinn_proto::Endpoint::new(endpoint_config, None, crate::allow_mtud());
        Self {
            endpoint,
            client_config,
            connections: HashMap::new(),
            udp_state: UdpState::new(),
        }
    }

    /// Create an endpoint for incoming connections, with the specified configs & local socket address.
    pub fn new_server(
        endpoint_config: EndpointConfig,
        server_config: ServerConfig,
        local_addr: SocketAddr,
    ) -> Self {
        let endpoint =
            quinn_proto::Endpoint::new(endpoint_config, Some(server_config), crate::allow_mtud());
        Self {
            endpoint,
            client_config: None,
            connections: HashMap::new(),
            udp_state: UdpState::new(),
        }
    }

    /// Create an endpoint for both incoming and outgoing connections, with the specified configs
    pub fn new_client_host(
        endpoint_config: EndpointConfig,
        client_config: Option<ClientConfig>,
        server_config: ServerConfig,
    ) {
        let endpoint =
            quinn_proto::Endpoint::new(endpoint_config, Some(server_config), crate::allow_mtud());
        Self {
            endpoint,
            client_config,
            connections: HashMap::new(),
            udp_state: UdpState::new(),
        }
    }

    /// Set the default client configuration used by [`Self::connect()`]
    pub fn set_default_client_config(&mut self, config: ClientConfig) {
        self.client_config = Some(config);
    }

    /// Replace the server configuration, affecting new incoming connections only
    pub fn set_server_config(&mut self, server_config: Option<ServerConfig>) {
        self.endpoint.set_server_config(server_config)
    }

    /// Initiate a connection to the server identified by the given [`SocketAddr`] and `server_name`, using the default config.
    /// The connection is added to the entity associated with the specified `EntityCommands`.
    ///
    /// The exact value of the `server_name` parameter must be included in the `subject_alt_names` field of the server's certificate,
    /// as described by [`config_with_gen_self_signed`]
    pub fn connect(
        &mut self,
        server_addr: SocketAddr,
        server_name: &str,
        entity: &mut EntityCommands,
    ) -> Result<(), ConnectError> {
        self.client_config
            .ok_or(ConnectError::NoDefaultClientConfig)
            .and_then(|config| self.connect_with(config, server_addr, server_name, entity))
    }

    /// Initiate a connection to the server identified by the given [`SocketAddr`] and `server_name`, using the given config.
    /// The connection is added to the entity associated with the specified `EntityCommands`.
    ///
    /// The exact value of the `server_name` parameter must be included in the `subject_alt_names` field of the server's certificate,
    /// as described by [`config_with_gen_self_signed`]
    pub fn connect_with(
        &mut self,
        config: ClientConfig,
        server_addr: SocketAddr,
        server_name: &str,
        entity: &mut EntityCommands,
    ) -> Result<(), ConnectError> {
        self.endpoint
            .connect(config, server_addr, server_name)
            .map(|(handle, connection)| {
                self.connections
                    .try_insert(handle, entity.id())
                    .expect("Got duplicate connection handle");
                entity.insert(Connection::new(handle, connection));
            })
    }

    /// Set the `socket_buffer_fill` to the input `len`
    fn set_socket_buffer_fill(&mut self, len: usize) {
        self.endpoint.set_socket_buffer_fill(len)
    }

    /// Reject new incoming connections without affecting existing connections
    ///
    /// Convenience short-hand for using [`set_server_config`](Self::set_server_config) to update
    /// [`concurrent_connections`](ServerConfig::concurrent_connections) to zero.
    pub fn reject_new_connections(&mut self) {
        self.endpoint.reject_new_connections()
    }

    /// Access the configuration used by this endpoint
    pub fn config(&self) -> &EndpointConfig {
        self.endpoint.config()
    }
}
