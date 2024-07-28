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
pub mod ip;
mod plugin;
mod socket;
pub mod streams;

/// If this component is placed on an entity, it will never be automatically despawned by this library
#[derive(Debug, Component)]
pub struct KeepAlive;
