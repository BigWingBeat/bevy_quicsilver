use bevy_ecs::{entity::Entity, event::Event, query::QueryEntityError};
use quinn_proto::{
    AcceptError, ConnectError, ConnectionError, ReadError, SendDatagramError, WriteError,
};
use rcgen::RcgenError;
use thiserror::Error;

pub use quinn_proto::{ClientConfig, ServerConfig};

mod client;
pub mod connection;
pub mod crypto;
mod endpoint;
pub mod ip;
mod plugin;
// mod server;
mod socket;

// pub(crate) fn allow_mtud() -> bool {
//     !quinn_udp::may_fragment()
// }

#[derive(Debug, Event)]
pub struct EntityError {
    pub entity: Entity,
    pub error: Error,
}

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
    #[error(transparent)]
    QueryEntity(#[from] QueryEntityError),
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    #[error(transparent)]
    Unreliable(#[from] SendDatagramError),
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
