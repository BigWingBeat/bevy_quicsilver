use std::{net::SocketAddr, sync::Arc};

use bevy::{
    app::{App, Plugin},
    ecs::system::Resource,
};
use coalescence_proto::{packet::OutboundPacket, serialize, Channel, ProtoPlugin};
use quinn_proto::{ConnectionHandle, Dir, EndpointConfig, StreamId};

use crate::{allow_mtud, connection::Connection, Error};

#[derive(Debug, Resource)]
struct ClientEndpoint {
    endpoint: quinn_proto::Endpoint,
    config: quinn_proto::ClientConfig,
}

#[derive(Debug)]
pub struct ClientPlugin {
    crypto: rustls::ClientConfig,
}

impl ClientPlugin {
    pub fn new(crypto: rustls::ClientConfig) -> Self {
        Self { crypto }
    }
}

impl Plugin for ClientPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ProtoPlugin);

        let endpoint = quinn_proto::Endpoint::new(EndpointConfig::default(), None, allow_mtud());
        let config = quinn_proto::ClientConfig::new(Arc::new(self.crypto));

        app.insert_resource(ClientEndpoint { endpoint, config });
    }
}

pub trait AppConnectToServerExt {
    fn connect_to_server(
        &mut self,
        server_addr: SocketAddr,
        server_name: &str,
    ) -> Result<(), Error>;
}

impl AppConnectToServerExt for App {
    /// Connect to the server identified by the given [`SocketAddr`]
    ///
    /// The exact value of the `server_name` parameter must be included in the `subject_alt_names` field of the server's certificate,
    /// as described by [`config_with_gen_self_signed`]
    fn connect_to_server(
        &mut self,
        server_addr: SocketAddr,
        server_name: &str,
    ) -> Result<(), Error> {
        let mut endpoint = self.world.resource_mut::<ClientEndpoint>();
        match endpoint
            .endpoint
            .connect(endpoint.config, server_addr, server_name)
        {
            Ok((handle, connection)) => {
                let ordered_stream = connection.streams().open(Dir::Bi).unwrap();
                self.world
                    .insert_resource(Connection::new(handle, connection, ordered_stream));
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}
