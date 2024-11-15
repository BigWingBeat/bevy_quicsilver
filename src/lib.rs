//! <style>
//! .rustdoc-hidden { display: none; }
//! </style>
#![doc = include_str!("../README.md")]
#![doc(test(attr(deny(warnings))))]

use bevy_ecs::{bundle::Bundle, component::Component, system::EntityCommands};

pub use quinn_proto as proto;
pub use quinn_proto::{
    ApplicationClose, ClientConfig, ClosedStream, ConnectError,
    ConnectionError as ConnectionLostError, EndpointConfig, FinishError, SendDatagramError,
    ServerConfig, StreamId, VarInt, VarIntBoundsExceeded, WriteError, Written,
};

mod connection;
pub use connection::{
    Connecting, ConnectingBundle, ConnectingError, Connection, ConnectionAccepted,
    ConnectionDrained, ConnectionError, ConnectionEstablished, HandshakeDataReady,
};

pub mod crypto;

mod endpoint;
pub use endpoint::{Endpoint, EndpointBundle, EndpointError};

mod incoming;
pub use incoming::{Incoming, IncomingError, IncomingResponse, NewIncoming};

mod plugin;
pub use plugin::QuicPlugin;

mod socket;

mod streams;
pub use streams::{RecvError, RecvStream, SendStream};

/// Automatically generated types for ECS queries.
pub mod query {
    pub use crate::connection::{
        ConnectingItem, ConnectionItem, ConnectionReadOnly, ConnectionReadOnlyItem,
    };
    pub use crate::endpoint::{EndpointItem, EndpointReadOnly, EndpointReadOnlyItem};
}

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
    use std::{any::TypeId, error::Error, net::Ipv6Addr, sync::Arc};

    use bevy_app::App;
    use bevy_ecs::{
        entity::Entity,
        observer::Trigger,
        query::{QueryData, With},
        system::{Query, ResMut, Resource},
    };
    use quinn_proto::{ClientConfig, ServerConfig};
    use rustls::{pki_types::PrivateKeyDer, RootCertStore};

    use crate::{
        connection::{ConnectingError, ConnectionError},
        endpoint::{EndpointBundle, EndpointError},
        incoming::IncomingError,
        Endpoint, Incoming, IncomingResponse, QuicPlugin,
    };

    #[derive(Debug)]
    pub(crate) struct ConnectionEntities {
        pub(crate) endpoint: Entity,
        pub(crate) client: Entity,
        pub(crate) server: Entity,
    }

    #[derive(Resource, Default)]
    pub(crate) struct HasObserverTriggered(pub(crate) bool);

    impl Drop for HasObserverTriggered {
        fn drop(&mut self) {
            if !self.0 {
                panic!("Observer was not triggered")
            }
        }
    }

    pub(crate) fn panic_on_trigger<T: Error>(trigger: Trigger<T>) {
        panic!(
            "Entity {} encountered an error: {}",
            trigger.entity(),
            trigger.event()
        );
    }

    pub(crate) fn test_observer<T, C: QueryData>(
    ) -> impl Fn(Trigger<T>, Query<C>, ResMut<HasObserverTriggered>) {
        |trigger: Trigger<T>, query: Query<C>, mut res: ResMut<HasObserverTriggered>| {
            let _ = query.get(trigger.entity()).unwrap();
            res.0 = true;
        }
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
            ServerConfig::with_single_cert(
                vec![key.cert.der().clone()],
                PrivateKeyDer::Pkcs8(key.key_pair.serialize_der().into()),
            )
            .unwrap(),
        )
    }

    pub(crate) fn endpoint() -> EndpointBundle {
        let (client, server) = generate_crypto();
        EndpointBundle::new_client_host((Ipv6Addr::LOCALHOST, 0).into(), client, server).unwrap()
    }

    pub(crate) fn incoming(app: &mut App) -> ConnectionEntities {
        let endpoint = endpoint();
        let endpoint_entity = app.world_mut().spawn(endpoint).id();

        let mut endpoint = app
            .world_mut()
            .query::<Endpoint>()
            .single_mut(app.world_mut());

        let addr = endpoint.local_addr().unwrap();

        let connecting = endpoint.connect(addr, "localhost").unwrap();
        let client = app.world_mut().spawn(connecting).id();

        app.update();
        app.update();

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

        app.update();
        app.update();
        app.update();

        entities
    }
}
