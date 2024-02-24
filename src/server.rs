use std::{net::SocketAddr, sync::Arc};

use bevy::{
    app::{App, Plugin, Update},
    ecs::{
        bundle::Bundle,
        component::Component,
        entity::Entity,
        event::{Event, EventWriter},
        system::{CommandQueue, Commands, Query, Resource},
        world::World,
    },
    log::info,
    tasks::IoTaskPool,
};
use coalescence_proto::ProtoPlugin;
use quinn_proto::{Endpoint, EndpointConfig};

use crate::{allow_mtud, ip::IPV6_WILDCARD_DEFAULT_PORT, Error};

#[derive(Debug, Resource)]
struct ServerEndpoint {
    endpoint: quinn_proto::Endpoint,
}

#[derive(Debug)]
pub struct ServerPlugin {
    crypto: rustls::ServerConfig,
}

impl ServerPlugin {
    pub fn new(crypto: rustls::ServerConfig) -> Self {
        Self { crypto }
    }
}

impl Plugin for ServerPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ProtoPlugin);

        let endpoint = Endpoint::new(
            EndpointConfig::default(),
            Some(quinn_proto::ServerConfig::with_crypto(Arc::new(
                self.crypto,
            ))),
            allow_mtud(),
        );

        app.insert_resource(ServerEndpoint { endpoint })
            .add_systems(Update, poll_accepted_connections);
    }
}

fn poll_accepted_connections(
    mut commands: Commands,
    mut query: Query<(Entity, &mut AcceptedConnections)>,
    mut errors: EventWriter<ConnectionFailed>,
) {
    for (entity, receiver) in query.iter_mut() {
        for accepted_connection in receiver.0.try_iter() {
            match accepted_connection {
                AcceptedConnection::Accepted(mut command_queue) => {
                    commands.append(&mut command_queue)
                }
                AcceptedConnection::Errored(error) => {
                    errors.send(ConnectionFailed { entity, error });
                }
            }
        }
    }
}

async fn accept_connections(endpoint: Endpoint, sender: Sender<AcceptedConnection>) {
    while let Some(connecting) = endpoint.accept().await {
        let address = connecting.remote_address();
        if let Some(local_ip) = connecting.local_ip() {
            info!("Incoming connection from '{address}' with local IP '{local_ip}'...");
        } else {
            info!("Incoming connection from '{address}'...");
        }
        IoTaskPool::get()
            .spawn(handle_connecting(connecting, sender.clone()))
            .detach();
    }
}

async fn handle_connecting(connecting: Connecting, sender: Sender<AcceptedConnection>) {
    // let address = connecting.remote_address();
    // let local_ip = connecting.local_ip();

    let result = match try_handle_connecting(connecting).await {
        Ok(command_queue) => AcceptedConnection::Accepted(command_queue),
        Err(e) => AcceptedConnection::Errored(e),
    };
    sender.send(result);
}

/// Separate function because try blocks are unstable
async fn try_handle_connecting(connecting: Connecting) -> Result<CommandQueue, Error> {
    let quinn_connection = connecting.await?;
    let (ordered_sender, ordered_receiver) = quinn_connection.accept_bi().await?;

    let mut command_queue = CommandQueue::default();
    command_queue.push(|world: &mut World| {
        world.spawn(AsyncConnection {
            quinn_connection,
            ordered_sender,
            ordered_receiver,
        });
    });
    Ok(command_queue)
}
