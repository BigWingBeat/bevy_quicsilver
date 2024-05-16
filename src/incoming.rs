use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use bevy_ecs::{
    component::Component,
    entity::Entity,
    event::{Event, EventWriter},
    query::{Added, QueryState, With},
    system::Query,
    world::World,
};
use quinn_proto::ServerConfig;

use crate::{
    connection::{ConnectionBundle, ConnectionImpl},
    endpoint::Endpoint,
};

/// Event raised whenever an endpoint receives a new incoming client connection
#[derive(Debug, Event)]
pub struct NewIncoming(pub Entity);

/// How to respond to an incoming client connection
///
/// # Usage
/// Insert this component onto an entity that has an [`Incoming`] component. If the response results in the connection being
/// accepted, the `Incoming` and `IncomingResponse` components will be removed from the entity, and a [`Connection`] component
/// will be added. Otherwise, the entity will be despawned.
#[derive(Debug, Component)]
pub enum IncomingResponse {
    /// Attempt to accept this incoming connection (an error may still occur).
    Accept,
    /// Attempt to accept this incoming connection, using a custom configuration
    AcceptWith(Arc<ServerConfig>),
    /// Reject this incoming connection.
    Refuse,
    /// Respond with a retry packet, requiring the client to retry with address validationz
    ///
    /// Errors if [`Incoming::remote_address_validated()`] is true
    Retry,
    /// Ignore this incoming connection attempt, not sending any packet in response
    Ignore,
}

/// A new incoming connection from a client
///
/// # Usage
/// ```
/// fn my_system(mut commands: Commands, incomings: Query<(Entity, &Incoming), Added<Incoming>>) {
///     for (entity, incoming) in incomings.iter() {
///         println!("New client connecting from {:?}", incoming.remote_address());
///         commands.entity(entity).insert(IncomingResponse::Accept);
///     }
/// }
/// ```
#[derive(Debug, Component)]
pub struct Incoming {
    pub(crate) incoming: quinn_proto::Incoming,
    pub(crate) endpoint_entity: Entity,
}

impl Incoming {
    /// The local IP address which was used when the peer established the connection
    ///
    /// See [`Connection::local_ip()`] for details
    pub fn local_ip(&self) -> Option<IpAddr> {
        self.incoming.local_ip()
    }

    /// The peer's UDP address
    pub fn remote_address(&self) -> SocketAddr {
        self.incoming.remote_address()
    }

    /// Whether the socket addess that is initiating this connection has been validated
    ///
    /// This means that the sender of the initial packet has proved that they can receive traffic sent to [`self.remote_address()`]
    pub fn remote_address_validated(&self) -> bool {
        self.incoming.remote_address_validated()
    }

    /// The entity that has the [`Endpoint`] component that is receiving this connection
    pub fn endpoint(&self) -> Entity {
        self.endpoint_entity
    }
}

/// Events are sent in system using [`Added`] instead of where the component is actually inserted,
/// because new events are visible immediately, but inserting components is deferred until `Commands` are applied
pub(crate) fn send_new_incoming_events(
    new_incomings: Query<Entity, Added<Incoming>>,
    mut events: EventWriter<NewIncoming>,
) {
    for entity in new_incomings.iter() {
        events.send(NewIncoming(entity));
    }
}

pub(crate) fn handle_incoming_responses(
    world: &mut World,
    endpoints: &mut QueryState<Endpoint>,
    incomings: &mut QueryState<Entity, (With<Incoming>, Added<IncomingResponse>)>,
) {
    let incomings = incomings.iter(world).collect::<Vec<_>>();
    for incoming_entity in incomings {
        let mut incoming_entity = world.entity_mut(incoming_entity);
        let (incoming, response) = incoming_entity
            .take::<(Incoming, IncomingResponse)>()
            .unwrap();

        let endpoint_entity = incoming.endpoint_entity;
        let incoming = incoming.incoming;

        let result = incoming_entity.world_scope(|world| {
            let mut endpoint = endpoints.get_mut(world, endpoint_entity).unwrap();
            match response {
                IncomingResponse::Accept => endpoint.accept(incoming, None).map(Some),
                IncomingResponse::AcceptWith(config) => {
                    endpoint.accept(incoming, Some(config)).map(Some)
                }
                IncomingResponse::Refuse => {
                    endpoint.refuse(incoming);
                    Ok(None)
                }
                IncomingResponse::Retry => endpoint.retry(incoming).map(|_| None),
                IncomingResponse::Ignore => {
                    endpoint.ignore(incoming);
                    Ok(None)
                }
            }
        });

        match result {
            Ok(Some((handle, connection))) => {
                incoming_entity.insert(ConnectionBundle::new(ConnectionImpl::new(
                    endpoint_entity,
                    handle,
                    connection,
                )));
            }
            Ok(None) => incoming_entity.despawn(),
            Err(e) => todo!(),
        }
    }
}
