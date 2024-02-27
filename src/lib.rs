use quinn_proto::{ConnectionError, ReadError, SendDatagramError, WriteError};
use rcgen::RcgenError;
use thiserror::Error;

pub use quinn_proto::{ClientConfig, ServerConfig};

mod client;
pub mod connection;
pub mod crypto;
mod endpoint;
pub mod ip;
mod plugin;
mod server;
mod socket;

pub(crate) fn allow_mtud() -> bool {
    !quinn_udp::may_fragment()
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
    Connection(#[from] ConnectionError),
    #[error(transparent)]
    Unreliable(#[from] SendDatagramError),
    #[error(transparent)]
    Read(#[from] ReadError),
    #[error(transparent)]
    Write(#[from] WriteError),
    #[error(transparent)]
    Rcgen(#[from] RcgenError),
    #[error(transparent)]
    Rustls(#[from] rustls::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
