use std::{net::SocketAddr, sync::Arc};

use bevy::{
    ecs::{
        entity::Entity,
        system::{CommandQueue, Resource},
    },
    utils::{default, EntityHashMap},
};
use quinn_proto::{ConnectionHandle, EndpointConfig};
use quinn_udp::UdpState;

use crate::{allow_mtud, connection::Connection, Error};

#[derive(Debug)]
pub(crate) struct ConnectionEvent {
    connection: ConnectionHandle,
    event: quinn_proto::ConnectionEvent,
}

#[derive(Debug, Resource)]
pub(crate) struct Endpoint {
    pub(crate) endpoint: quinn_proto::Endpoint,
    pub(crate) connections: EntityHashMap<ConnectionHandle, Entity>,
    pub(crate) udp_state: UdpState,
}

impl Endpoint {
    /// Returns a `CommandQueue` that will construct a new endpoint for outgoing connections, with the specified crypto config,
    /// and use that endpoint to connect to the server identified by the given `SocketAddr`.
    ///
    /// The exact value of the `server_name` parameter must be included in the `subject_alt_names` field of the server's certificate,
    /// as described by [`config_with_gen_self_signed`]
    pub fn new_client(
        crypto: rustls::ClientConfig,
        server_addr: SocketAddr,
        server_name: &str,
    ) -> Result<CommandQueue, Error> {
        let endpoint = Endpoint::new(EndpointConfig::default(), None, allow_mtud());
        let config = quinn_proto::ClientConfig::new(Arc::new(crypto));

        endpoint
            .connect(config, server_addr, server_name)
            .map(|(id, connection)| {
                let queue = CommandQueue::default();
                queue.push(|&mut world| {
                    world.insert_resource(endpoint);
                    world.spawn(ConnectionBundle::<Client> {
                        connection: Connection { id, connection },
                        proto: default(),
                    })
                });
                queue
            })
            .map_err(Into::into)
    }

    /// Returns a `CommandQueue` that will construct a new endpoint for incoming connections,
    /// with the specified local socket address and crypto config.
    pub fn new_server(local_addr: SocketAddr, crypto: rustls::ServerConfig) -> CommandQueue {
        let endpoint = Endpoint::new(
            EndpointConfig::default(),
            Some(quinn_proto::ServerConfig::with_crypto(Arc::new(crypto))),
            allow_mtud(),
        );
        let mut queue = CommandQueue::default();
        queue.push(|&mut world| world.insert_resource(endpoint));
        queue
    }
}
