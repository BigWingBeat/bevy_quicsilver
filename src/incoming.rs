use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use bevy_ecs::{
    component::{Component, ComponentHooks, StorageType},
    entity::Entity,
    event::{Event, EventReader, EventWriter},
    query::{QueryEntityError, QueryState},
    system::SystemState,
    world::World,
};
use quinn_proto::ServerConfig;

use crate::{
    connection::{ConnectingBundle, ConnectionImpl},
    endpoint::Endpoint,
    KeepAlive,
};

/// Event raised whenever an endpoint receives a new incoming client connection.
/// The specified entity will have an [`Incoming`] component
#[derive(Debug, Event)]
pub struct NewIncoming(pub Entity);

#[derive(Debug, Clone)]
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
/// # use bevy_app::{App, Update};
/// # use bevy_ecs::prelude::{Query, EventReader, EventWriter};
/// # use bevy_quicsilver::{QuicPlugin, Incoming, NewIncoming, IncomingResponse};
///
/// # let mut app = App::new();
/// # app.add_plugins(QuicPlugin);
/// # app.add_systems(Update, my_system);
/// # app.update();
///
/// fn my_system(
///     query: Query<&Incoming>,
///     mut incomings: EventReader<NewIncoming>,
///     mut responses: EventWriter<IncomingResponse>,
/// ) {
///     for &NewIncoming(entity) in incomings.read() {
///         let incoming = query.get(entity).unwrap();
///         println!("New client connecting from {:?}", incoming.remote_address());
///         responses.send(IncomingResponse::accept(entity));
///     }
/// }
/// ```
#[derive(Debug)]
pub struct Incoming {
    pub(crate) incoming: quinn_proto::Incoming,
    pub(crate) endpoint_entity: Entity,
}

impl Component for Incoming {
    const STORAGE_TYPE: StorageType = StorageType::Table;

    fn register_component_hooks(hooks: &mut ComponentHooks) {
        hooks.on_add(|mut world, entity, _component_id| {
            world.send_event(NewIncoming(entity));
        });
    }
}

impl Incoming {
    /// The local IP address which was used when the peer established the connection.
    ///
    /// This can be different from the address the endpoint is bound to, in case
    /// the endpoint is bound to a wildcard address like `0.0.0.0` or `::`.
    ///
    /// This will return `None` for clients, or when the platform does not expose this
    /// information. See [`quinn_udp::RecvMeta::dst_ip`] for a list of supported platforms
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
        let Some(mut incoming_entity) = world.get_entity_mut(response.entity) else {
            error_events
                .get_mut(world)
                .send(EntityError::new(response.entity, ErrorKind::NoSuchEntity));
            continue;
        };

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
            let mut endpoint = match endpoints.get_mut(world, endpoint_entity) {
                Ok(endpoint) => endpoint,
                Err(QueryEntityError::QueryDoesNotMatch(_)) // If the endpoint does not exist anymore, neither should we
                | Err(QueryEntityError::NoSuchEntity(_)) => return Ok(None),
                Err(QueryEntityError::AliasedMutability(_)) => unreachable!(),
            };

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
            // Connection successfully accepted
            Ok(Some((handle, connection))) => {
                incoming_entity.insert(ConnectingBundle::new(ConnectionImpl::new(
                    endpoint_entity,
                    handle,
                    connection,
                )));
            }
            // Connection refused, retried or ignored
            Ok(None) => {
                if !incoming_entity.contains::<KeepAlive>() {
                    incoming_entity.despawn();
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
