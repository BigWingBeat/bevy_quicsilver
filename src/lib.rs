use bevy_ecs::{component::Component, entity::Entity, event::Event};
use quinn_proto::{
    ConnectError, ConnectionError, ReadError, RetryError, SendDatagramError, WriteError,
};
use thiserror::Error;

pub use quinn_proto::{ClientConfig, ServerConfig};

pub mod connection;
pub mod crypto;
pub mod endpoint;
pub mod incoming;
pub mod ip;
mod plugin;
mod socket;

/// If this component is placed on an entity, it will never be automatically despawned by this library
#[derive(Debug, Component)]
pub struct KeepAlive;

/// Event raised whenever a library error occurs that pertains to a particular entity
#[derive(Debug, Event)]
pub struct EntityError {
    pub entity: Entity,
    pub error: Error,
}

impl EntityError {
    pub(crate) fn new(entity: Entity, error: impl Into<Error>) -> Self {
        Self {
            entity,
            error: error.into(),
        }
    }
}

/// Opaque library error type
#[derive(Debug, Error)]
#[error(transparent)]
pub struct Error(ErrorKind);

impl<T> From<T> for Error
where
    T: Into<ErrorKind>,
{
    fn from(error: T) -> Self {
        Self(error.into())
    }
}

#[derive(Debug, Error)]
pub(crate) enum ErrorKind {
    #[error("The stream was finished unexpectedly")]
    StreamFinished,
    #[error("Expected entity to have a component of type {0}, but it does not")]
    MissingComponent(&'static str),
    #[error(transparent)]
    Retry(#[from] RetryError),
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    #[error(transparent)]
    SendDatagram(#[from] SendDatagramError),
    #[error(transparent)]
    Read(#[from] ReadError),
    #[error(transparent)]
    Write(#[from] WriteError),
    #[error(transparent)]
    Rcgen(#[from] rcgen::Error),
    #[error(transparent)]
    Rustls(#[from] rustls::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl ErrorKind {
    pub(crate) fn missing_component<T>() -> Self {
        Self::MissingComponent(std::any::type_name::<T>())
    }
}
