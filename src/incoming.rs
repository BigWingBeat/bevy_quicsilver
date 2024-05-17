use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use bevy_ecs::{
    component::Component,
    entity::Entity,
    event::{Event, EventReader, EventWriter},
    query::{Added, QueryState},
    system::{Query, SystemState},
    world::World,
};
use quinn_proto::ServerConfig;

use crate::{
    connection::{ConnectionBundle, ConnectionImpl},
    endpoint::Endpoint,
    EntityError, ErrorKind, KeepAlive,
};

/// Event raised whenever an endpoint receives a new incoming client connection.
/// The specified entity will have an [`Incoming`] component
#[derive(Debug, Event)]
pub struct NewIncoming(pub Entity);

#[derive(Debug, Clone, Component)]
enum IncomingResponseType {
    Accept(Option<Arc<ServerConfig>>),
    Refuse,
    Retry,
    Ignore,
}

/// How to respond to an incoming client connection.
///
/// Errors if the specified entity does not have an [`Incoming`] component
#[derive(Debug, Clone, Event)]
pub struct IncomingResponse {
    entity: Entity,
    response: IncomingResponseType,
}

impl IncomingResponse {
    /// Attempt to accept this incoming connection. If no errors occur, the [`Incoming`] component on the specified entity will
    /// be replaced with a [`Connecting`] component
    pub fn accept(entity: Entity) -> Self {
        Self {
            entity,
            response: IncomingResponseType::Accept(None),
        }
    }

    /// Attempt to accept this incoming connection, using a custom configuration.
    /// If no errors occur, the [`Incoming`] component on the specified entity will be replaced with a [`Connecting`] component
    pub fn accept_with(entity: Entity, config: Arc<ServerConfig>) -> Self {
        Self {
            entity,
            response: IncomingResponseType::Accept(Some(config)),
        }
    }

    /// Reject this incoming connection. The specified entity will be despawned, unless it has a [`KeepAlive`] component
    pub fn refuse(entity: Entity) -> Self {
        Self {
            entity,
            response: IncomingResponseType::Refuse,
        }
    }

    /// Respond with a retry packet, requiring the client to retry with address validation.
    ///
    /// Errors if [`Incoming::remote_address_validated()`] is true,
    /// otherwise despawns the specified entity, unless it has a [`KeepAlive`] component
    pub fn retry(entity: Entity) -> Self {
        Self {
            entity,
            response: IncomingResponseType::Retry,
        }
    }

    /// Ignore this incoming connection attempt, not sending any packet in response.
    /// The specified entity will be despawned, unless it has a [`KeepAlive`] component
    pub fn ignore(entity: Entity) -> Self {
        Self {
            entity,
            response: IncomingResponseType::Ignore,
        }
    }
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
    response_events: &mut SystemState<EventReader<IncomingResponse>>,
    error_events: &mut SystemState<EventWriter<EntityError>>,
) {
    let responses = response_events
        .get_mut(world)
        .read()
        .cloned()
        .collect::<Vec<_>>();

    for response in responses {
        let mut incoming_entity = world.entity_mut(response.entity);
        let incoming_entity_id = incoming_entity.id();

        let Some(incoming) = incoming_entity.take::<Incoming>() else {
            error_events.get_mut(world).send(EntityError::new(
                incoming_entity_id,
                ErrorKind::missing_component::<Incoming>(),
            ));
            continue;
        };

        let endpoint_entity = incoming.endpoint_entity;
        let incoming = incoming.incoming;

        let result = incoming_entity.world_scope(|world| {
            let mut endpoint = endpoints.get_mut(world, endpoint_entity).unwrap();
            match response.response {
                IncomingResponseType::Accept(config) => endpoint.accept(incoming, config).map(Some),
                IncomingResponseType::Refuse => {
                    endpoint.refuse(incoming);
                    Ok(None)
                }
                IncomingResponseType::Retry => endpoint.retry(incoming).map(|_| None),
                IncomingResponseType::Ignore => {
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
            Ok(None) => {
                if !incoming_entity.contains::<KeepAlive>() {
                    incoming_entity.despawn()
                }
            }
            Err(error) => {
                error_events
                    .get_mut(world)
                    .send(EntityError::new(incoming_entity_id, error));
            }
        }
    }
}
