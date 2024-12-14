//! <style>
//! .rustdoc-hidden { display: none; }
//! </style>
#![doc = include_str!("../README.md")]
#![doc(test(attr(deny(warnings))))]

use bevy_ecs::{bundle::Bundle, component::Component, system::EntityCommands};

pub use quinn_proto as proto;
pub use quinn_proto::{
    ApplicationClose, ClientConfig, ClosedStream, ConnectionError as ConnectionLostError,
    EndpointConfig, FinishError, SendDatagramError, ServerConfig, StreamId, VarInt,
    VarIntBoundsExceeded, WriteError, Written,
};

mod connection;
pub use connection::{
    Connecting, ConnectingError, Connection, ConnectionAccepted, ConnectionDrained,
    ConnectionError, ConnectionEstablished, HandshakeDataReady,
};

pub mod crypto;

mod endpoint;
pub use endpoint::{ConnectError, Endpoint, EndpointError};

mod incoming;
pub use incoming::{Incoming, IncomingError, IncomingResponse, NewIncoming};

mod plugin;
pub use plugin::QuicPlugin;

mod socket;

mod streams;
pub use streams::{RecvError, RecvStream, SendStream};

/// If this component is placed on an entity, it will never be automatically despawned by this library.
/// For example, closing a connection normally results in the entity being despawned, but if this component
/// is on the entity, instead only the connection components will be removed from the entity, and it will not be despawned.
#[derive(Debug, Component)]
pub struct KeepAlive;

trait KeepAliveEntityCommandsExt {
    fn remove_or_despawn<B: Bundle>(&mut self, keepalive: bool);
}

impl KeepAliveEntityCommandsExt for EntityCommands<'_> {
    fn remove_or_despawn<B: Bundle>(&mut self, keepalive: bool) {
        if keepalive {
            self.remove::<B>();
        } else {
            self.despawn();
        }
    }
}

/// Helper functions for tests
#[cfg(test)]
mod tests {
    use std::{
        any::TypeId,
        error::Error,
        net::Ipv6Addr,
        sync::{Arc, Once},
    };

    use bevy_app::App;
    use bevy_ecs::{
        entity::Entity, event::Event, observer::Trigger, query::With, system::IntoObserverSystem,
    };
    use quinn_proto::{ClientConfig, ServerConfig};
    use rustls::RootCertStore;

    use crate::{
        connection::{ConnectingError, ConnectionError},
        crypto::ServerConfigExt,
        endpoint::EndpointError,
        incoming::IncomingError,
        ConnectionEstablished, Endpoint, Incoming, IncomingResponse, NewIncoming, QuicPlugin,
    };

    #[derive(Debug)]
    pub(crate) struct ConnectionEntities {
        pub(crate) endpoint: Entity,
        pub(crate) client: Entity,
        pub(crate) server: Entity,
    }

    pub(crate) fn panic_on_trigger<T: Error>(trigger: Trigger<T>) {
        panic!(
            "Entity {} encountered an error: {}",
            trigger.entity(),
            trigger.event()
        );
    }

    pub(crate) fn wait_for_observer<E: Event, M>(
        app: &mut App,
        entity: Option<Entity>,
        observer: impl IntoObserverSystem<E, (), M>,
        expected_updates: Option<u16>,
        otherwise: &str,
    ) {
        let has_run = Arc::new(Once::new());

        {
            let has_run = has_run.clone();
            if let Some(entity) = entity {
                app.world_mut()
                    .entity_mut(entity)
                    .observe(observer)
                    .observe(move |_trigger: Trigger<E>| {
                        has_run.call_once(|| {});
                    });
            } else {
                app.world_mut().add_observer(observer);
                app.world_mut().add_observer(move |_trigger: Trigger<E>| {
                    has_run.call_once(|| {});
                });
            }
        }

        // If `Some`, the observer should trigger in an exact number of updates.
        // If `None`, it's inconsistent (I/O etc.) and just needs to trigger in a reasonable amount of time
        for i in 0..expected_updates.unwrap_or(u16::MAX) {
            app.update();
            if has_run.is_completed() {
                // i here is 1 less than the actual number of times this loop runs
                if expected_updates.is_some_and(|updates| updates != (i + 1)) {
                    panic!("{}", otherwise);
                }
                return;
            }
        }

        panic!("{}", otherwise);
    }

    pub(crate) fn app() -> App {
        let mut app = App::new();
        app.add_plugins(QuicPlugin);
        app
    }

    pub(crate) fn app_no_errors() -> App {
        let mut app = app();
        app.add_observer(panic_on_trigger::<EndpointError>)
            .add_observer(panic_on_trigger::<ConnectingError>)
            .add_observer(panic_on_trigger::<IncomingError>)
            .add_observer(panic_on_trigger::<ConnectionError>);
        app
    }

    pub(crate) fn app_one_error<T: 'static>() -> App {
        let mut app = app();
        let t = TypeId::of::<T>();
        if t != TypeId::of::<EndpointError>() {
            app.add_observer(panic_on_trigger::<EndpointError>);
        }

        if t != TypeId::of::<ConnectingError>() {
            app.add_observer(panic_on_trigger::<ConnectingError>);
        }

        if t != TypeId::of::<IncomingError>() {
            app.add_observer(panic_on_trigger::<IncomingError>);
        }

        if t != TypeId::of::<ConnectionError>() {
            app.add_observer(panic_on_trigger::<ConnectionError>);
        }
        app
    }

    pub(crate) fn generate_crypto() -> (ClientConfig, ServerConfig) {
        let key = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let mut roots = RootCertStore::empty();
        roots.add(key.cert.der().clone()).unwrap();

        (
            ClientConfig::with_root_certificates(Arc::new(roots)).unwrap(),
            ServerConfig::with_rcgen_cert(key).unwrap(),
        )
    }

    pub(crate) fn endpoint() -> Endpoint {
        let (client, server) = generate_crypto();
        Endpoint::new_client_host((Ipv6Addr::LOCALHOST, 0).into(), client, server).unwrap()
    }

    pub(crate) fn incoming(app: &mut App) -> ConnectionEntities {
        let endpoint = endpoint();
        let endpoint_entity = app.world_mut().spawn(endpoint).id();

        let mut endpoint = app
            .world_mut()
            .query::<&mut Endpoint>()
            .single_mut(app.world_mut());

        let addr = endpoint.local_addr().unwrap();

        let connecting = endpoint.connect(addr, "localhost").unwrap();
        let client = app.world_mut().spawn(connecting).id();

        wait_for_observer(
            app,
            None,
            |_: Trigger<NewIncoming>| {},
            None,
            "Incoming did not spawn",
        );

        let server = app
            .world_mut()
            .query_filtered::<Entity, With<Incoming>>()
            .single_mut(app.world_mut());

        ConnectionEntities {
            endpoint: endpoint_entity,
            client,
            server,
        }
    }

    pub(crate) fn connection(app: &mut App) -> ConnectionEntities {
        let entities = incoming(app);

        app.world_mut()
            .send_event(IncomingResponse::accept(entities.server));

        // Client establishes first, sends packet to server, then server establishes second.
        // So, wait for server to establish, then we know both client and server are established
        wait_for_observer(
            app,
            Some(entities.server),
            |_: Trigger<ConnectionEstablished>| {},
            None,
            "ConnectionEstablished did not fire",
        );

        entities
    }
}
