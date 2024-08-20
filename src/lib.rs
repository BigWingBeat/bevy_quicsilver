#![doc(test(attr(deny(warnings))))]
#![doc = include_str!("../README.md")]

use bevy_ecs::component::Component;

pub use quinn_proto::{ClientConfig, ServerConfig};

pub use endpoint::Endpoint;
pub use incoming::{Incoming, IncomingResponse, NewIncoming};
pub use plugin::QuicPlugin;

pub mod connection;
pub mod crypto;
pub mod endpoint;
pub mod incoming;
mod plugin;
mod socket;
pub mod streams;

/// If this component is placed on an entity, it will never be automatically despawned by this library.
/// For example, closing a connection normally results in the entity being despawned, but if this component
/// is on the entity, instead only the connection components will be removed from the entity, and it will not be despawned
#[derive(Debug, Component)]
pub struct KeepAlive;

/// Helper functions for tests located in specific modules
#[cfg(test)]
mod tests {
    use std::{error::Error, net::Ipv6Addr, sync::Arc};

    use bevy_app::App;
    use bevy_ecs::observer::Trigger;
    use quinn_proto::{ClientConfig, ServerConfig};
    use rustls::{pki_types::PrivateKeyDer, RootCertStore};

    use crate::{
        connection::{ConnectingError, ConnectionError},
        endpoint::{EndpointBundle, EndpointError},
        incoming::IncomingError,
        QuicPlugin,
    };

    pub(crate) fn panic_on_trigger<T: Error>(trigger: Trigger<T>) {
        panic!(
            "Entity {} encountered an error: {}",
            trigger.entity(),
            trigger.event()
        );
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

    pub(crate) fn app_no_errors() -> App {
        let mut app = App::new();
        app.add_plugins(QuicPlugin)
            .observe(panic_on_trigger::<EndpointError>)
            .observe(panic_on_trigger::<ConnectingError>)
            .observe(panic_on_trigger::<IncomingError>)
            .observe(panic_on_trigger::<ConnectionError>);
        app
    }
}
