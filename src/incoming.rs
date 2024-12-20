use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use bevy_ecs::{
    component::Component,
    entity::Entity,
    event::{Event, EventReader},
    system::SystemState,
    world::{error::EntityFetchError, World},
};
use quinn_proto::{ConnectionError, ServerConfig};
use thiserror::Error;

use crate::{connection::ConnectionAccepted, endpoint::Endpoint, Connecting, KeepAlive};

/// An observer trigger that is fired whenever an [`Incoming`] component on an entity encounters an error.
///
/// Be aware that the target entity will never have an [`Incoming`] component when this is fired.
#[derive(Debug, Error, Event)]
pub enum IncomingError {
    /// An [`IncomingResponse`] event was raised for an entity that does not have an [`Incoming`] component
    #[error("The entity does not have an {} component", std::any::type_name::<Incoming>())]
    MalformedEntity,
    /// An [`IncomingResponse`] event was raised for an entity that does not exist
    #[error("The entity does not exist")]
    MissingEntity,
    /// An error occurred when attempting to accept the connection
    #[error(transparent)]
    AcceptError(ConnectionError),
    /// Attempted to retry an [`Incoming`] which already bears an address validation token from a previous retry
    #[error("Attempted to retry a client which already bears an address validation token from a previous retry")]
    RetryError,
}

/// An observer trigger that is fired whenever an endpoint receives a new incoming client connection.
/// The specified entity will have an [`Incoming`] component.
#[derive(Debug, Event)]
pub struct NewIncoming;

/// An event type that specifies how to respond to an incoming client connection.
///
/// Once a response has been sent for an entity, the [`Incoming`] component will be removed, and the response will be handled.
///
/// Errors if the specified entity does not exist, or does not have an [`Incoming`] component.
#[derive(Debug, Clone, Event)]
#[must_use = "IncomingResponse is an event type and does nothing if not written to an EventWriter"]
pub struct IncomingResponse {
    entity: Entity,
    response: IncomingResponseType,
}

#[derive(Debug, Clone)]
enum IncomingResponseType {
    Accept(Option<Arc<ServerConfig>>),
    Refuse,
    Retry,
    Ignore,
}

impl IncomingResponse {
    /// Attempt to accept this incoming connection. If no errors occur, the [`Incoming`] component on the specified entity will
    /// be replaced with a [`Connecting`](crate::Connecting) component, and a [`ConnectionAccepted`] event will be fired.
    ///
    /// If an error occurrs, an [`IncomingError`] trigger will be fired, and then, if the entity does not have a [`KeepAlive`]
    /// component, it will be despawned.
    pub fn accept(entity: Entity) -> Self {
        Self {
            entity,
            response: IncomingResponseType::Accept(None),
        }
    }

    /// Attempt to accept this incoming connection, using a custom configuration.
    /// If no errors occur, the [`Incoming`] component on the specified entity will be replaced with a [`Connecting`](crate::Connecting)
    /// component, and a [`ConnectionAccepted`] event will be fired.
    ///
    /// If an error occurrs, an [`IncomingError`] trigger will be fired, and then, if the entity does not have a [`KeepAlive`]
    /// component, it will be despawned.
    pub fn accept_with(entity: Entity, config: Arc<ServerConfig>) -> Self {
        Self {
            entity,
            response: IncomingResponseType::Accept(Some(config)),
        }
    }

    /// Reject this incoming connection. The specified entity will be despawned, unless it has a [`KeepAlive`] component,
    /// in which case only the [`Incoming`] component will be removed from it.
    pub fn refuse(entity: Entity) -> Self {
        Self {
            entity,
            response: IncomingResponseType::Refuse,
        }
    }

    /// Respond with a retry packet, requiring the client to retry with address validation.
    ///
    /// If [`Incoming::remote_address_validated()`] is true, an [`IncomingError`] trigger will be fired,
    /// and no retry packet will be sent.
    ///
    /// Regardless of any errors, the specified entity will be despawned, unless it has a [`KeepAlive`] component,
    /// in which case only the [`Incoming`] component will be removed from it.
    pub fn retry(entity: Entity) -> Self {
        Self {
            entity,
            response: IncomingResponseType::Retry,
        }
    }

    /// Ignore this incoming connection attempt, not sending any packet in response.
    /// The specified entity will be despawned, unless it has a [`KeepAlive`] component,
    /// in which case only the [`Incoming`] component will be removed from it.
    pub fn ignore(entity: Entity) -> Self {
        Self {
            entity,
            response: IncomingResponseType::Ignore,
        }
    }
}

/// A new incoming connection from a client. Handle this by sending an [`IncomingResponse`] event.
///
/// # Usage
/// ```
/// # use bevy_ecs::prelude::{Query, EventWriter, Trigger};
/// # use bevy_ecs::system::assert_is_system;
/// # use bevy_quicsilver::{Incoming, NewIncoming, IncomingResponse};
/// fn my_observer(
///     trigger: Trigger<NewIncoming>,
///     query: Query<&Incoming>,
///     mut responses: EventWriter<IncomingResponse>,
/// ) {
///     let entity = trigger.entity();
///     let incoming = query.get(entity).unwrap();
///     if incoming.remote_address_validated() {
///         println!("New client connecting from {}", incoming.remote_address());
///         responses.send(IncomingResponse::accept(entity));
///     } else {
///         responses.send(IncomingResponse::retry(entity));
///     }
/// }
/// # assert_is_system(my_observer);
/// ```
#[derive(Debug, Component)]
pub struct Incoming {
    incoming: quinn_proto::Incoming,
    endpoint_entity: Entity,
}

impl Incoming {
    pub(crate) fn new(incoming: quinn_proto::Incoming, endpoint_entity: Entity) -> Self {
        Self {
            incoming,
            endpoint_entity,
        }
    }

    /// The local IP address which was used when the peer established the connection.
    ///
    /// This can be different from the address the endpoint is bound to, in case
    /// the endpoint is bound to a wildcard address like `0.0.0.0` or `::`.
    ///
    /// This will return `None` for clients, or when the platform does not expose this
    /// information. See [`quinn_udp::RecvMeta::dst_ip`] for a list of supported platforms.
    pub fn local_ip(&self) -> Option<IpAddr> {
        self.incoming.local_ip()
    }

    /// The peer's UDP address.
    pub fn remote_address(&self) -> SocketAddr {
        self.incoming.remote_address()
    }

    /// Whether the socket addess that is initiating this connection has been validated.
    ///
    /// This means that the sender of the initial packet has proved that they can receive traffic sent to [`self.remote_address()`].
    pub fn remote_address_validated(&self) -> bool {
        self.incoming.remote_address_validated()
    }

    /// The entity that has the [`Endpoint`] component that is receiving this connection.
    pub fn endpoint(&self) -> Entity {
        self.endpoint_entity
    }
}

pub(crate) fn handle_incoming_responses(
    world: &mut World,
    response_events: &mut SystemState<EventReader<IncomingResponse>>,
) {
    let responses = response_events
        .get_mut(world)
        .read()
        .cloned()
        .collect::<Vec<_>>();

    for response in responses {
        let mut incoming_entity = match world.get_entity_mut(response.entity) {
            Ok(incoming_entity) => incoming_entity,
            Err(EntityFetchError::NoSuchEntity(entity)) => {
                world.trigger_targets(IncomingError::MissingEntity, entity);
                continue;
            }
            Err(EntityFetchError::AliasedMutability(_)) => unreachable!(),
        };

        let incoming_entity_id = incoming_entity.id();

        // Remove the Incoming component immediately, as there are no responses that retain it
        let Some(incoming) = incoming_entity.take::<Incoming>() else {
            world.trigger_targets(IncomingError::MalformedEntity, incoming_entity_id);
            continue;
        };

        let endpoint_entity = incoming.endpoint_entity;
        let incoming = incoming.incoming;

        let result = incoming_entity.world_scope(|world| {
            let mut endpoint = match world.get_mut::<Endpoint>(endpoint_entity) {
                Some(endpoint) => endpoint,
                None => {
                    // If the endpoint does not exist anymore, neither should we
                    return Ok(None);
                }
            };

            match response.response {
                IncomingResponseType::Accept(config) => endpoint
                    .accept(incoming, config)
                    .map(Some)
                    .map_err(IncomingError::AcceptError),
                IncomingResponseType::Refuse => {
                    endpoint.refuse(incoming);
                    Ok(None)
                }
                IncomingResponseType::Retry => endpoint
                    .retry(incoming)
                    .map(|()| None)
                    .map_err(|_| IncomingError::RetryError),
                IncomingResponseType::Ignore => {
                    endpoint.ignore(incoming);
                    Ok(None)
                }
            }
        });

        match result {
            // Connection successfully accepted
            Ok(Some((handle, connection))) => {
                incoming_entity.insert(Connecting::new(endpoint_entity, handle, connection));
                incoming_entity.world_scope(|world| {
                    world.trigger_targets(ConnectionAccepted, incoming_entity_id);
                });
            }
            // Connection refused, retried or ignored
            Ok(None) => {
                if !incoming_entity.contains::<KeepAlive>() {
                    incoming_entity.despawn();
                }
            }
            // Connection failed
            Err(error) => {
                incoming_entity.trigger(error);
                if !incoming_entity.contains::<KeepAlive>() {
                    incoming_entity.despawn();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy_ecs::{entity::Entity, observer::Trigger, query::With};

    use crate::{tests::*, Endpoint, KeepAlive};

    use super::{Incoming, IncomingError, IncomingResponse, NewIncoming};

    #[test]
    fn malformed_entity_error() {
        let mut app = app_one_error::<IncomingError>();

        let entity = app.world_mut().spawn_empty().id();

        app.world_mut().send_event(IncomingResponse::accept(entity));

        wait_for_observer(
            &mut app,
            Some(entity),
            |trigger: Trigger<IncomingError>| {
                assert!(matches!(trigger.event(), IncomingError::MalformedEntity));
            },
            Some(1),
            "IncomingError did not trigger",
        );
    }

    #[test]
    fn missing_entity_error() {
        let mut app = app_one_error::<IncomingError>();

        let entity = Entity::PLACEHOLDER;

        app.world_mut().send_event(IncomingResponse::accept(entity));

        wait_for_observer(
            &mut app,
            None,
            move |trigger: Trigger<IncomingError>| {
                assert_eq!(trigger.entity(), entity);
                assert!(matches!(trigger.event(), IncomingError::MissingEntity));
            },
            Some(1),
            "IncomingError did not trigger",
        );
    }

    #[test]
    fn retry_error() {
        let mut app = app_one_error::<IncomingError>();

        let connections = incoming(&mut app);

        app.world_mut()
            .send_event(IncomingResponse::retry(connections.server));

        wait_for_observer(
            &mut app,
            None,
            |_: Trigger<NewIncoming>| {},
            None,
            "Incoming did not reappear after retry",
        );

        let incoming = app
            .world_mut()
            .query_filtered::<Entity, With<Incoming>>()
            .single(app.world());

        app.world_mut()
            .send_event(IncomingResponse::retry(incoming));

        wait_for_observer(
            &mut app,
            Some(incoming),
            |trigger: Trigger<IncomingError>| {
                assert!(matches!(trigger.event(), IncomingError::RetryError));
            },
            Some(1),
            "IncomingError did not trigger",
        );
    }

    #[test]
    fn new_incoming() {
        let mut app = app_no_errors();

        let endpoint = endpoint();
        app.world_mut().spawn(endpoint);

        let mut endpoint = app
            .world_mut()
            .query::<&mut Endpoint>()
            .single_mut(app.world_mut());

        let addr = endpoint.local_addr().unwrap();

        let connecting = endpoint.connect(addr, "localhost").unwrap();
        app.world_mut().spawn(connecting);

        wait_for_observer(
            &mut app,
            None,
            |_: Trigger<NewIncoming>| {},
            None,
            "NewIncoming did not trigger",
        );
    }

    #[test]
    fn keepalive() {
        let mut app = app_no_errors();

        let connections = incoming(&mut app);

        app.world_mut()
            .entity_mut(connections.server)
            .insert(KeepAlive);

        app.world_mut()
            .send_event(IncomingResponse::ignore(connections.server));

        app.update();

        assert!(!app
            .world()
            .entity(connections.server)
            .contains::<Incoming>());
    }

    #[test]
    fn no_keepalive() {
        let mut app = app_no_errors();

        let connections = incoming(&mut app);

        app.world_mut()
            .send_event(IncomingResponse::ignore(connections.server));

        app.update();

        assert!(app
            .world_mut()
            .get_entity(connections.server)
            .is_err_and(|entity| entity == connections.server));
    }
}
