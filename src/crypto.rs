//! Traits and implementations for the QUIC cryptography protocol
//!
//! The protocol logic in Quinn is contained in types that abstract over the actual
//! cryptographic protocol used. This module contains the traits used for this
//! abstraction layer as well as a single implementation of these traits that uses
//! *ring* and rustls to implement the TLS protocol support.
//!
//! Note that usage of any protocol (version) other than TLS 1.3 does not conform to any
//! published versions of the specification, and will not be supported in QUIC v1.

use std::sync::Arc;

use quinn_proto::crypto::rustls::{NoInitialCipherSuite, QuicClientConfig, QuicServerConfig};

pub use quinn_proto::crypto::*;

/// Extension trait to make it easier to provide a custom crypto config
pub trait CryptoConfigExt: Sized {
    type RustlsConfig;

    fn with_rustls_config(config: Self::RustlsConfig) -> Result<Self, NoInitialCipherSuite>;
}

impl CryptoConfigExt for quinn_proto::ClientConfig {
    type RustlsConfig = ::rustls::ClientConfig;

    fn with_rustls_config(config: Self::RustlsConfig) -> Result<Self, NoInitialCipherSuite> {
        config
            .try_into()
            .map(|config: QuicClientConfig| Self::new(Arc::new(config)))
    }
}

impl CryptoConfigExt for quinn_proto::ServerConfig {
    type RustlsConfig = ::rustls::ServerConfig;

    fn with_rustls_config(config: Self::RustlsConfig) -> Result<Self, NoInitialCipherSuite> {
        config
            .try_into()
            .map(|config: QuicServerConfig| Self::with_crypto(Arc::new(config)))
    }
}
