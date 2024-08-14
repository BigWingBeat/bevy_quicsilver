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
