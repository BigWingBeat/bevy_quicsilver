use std::sync::Arc;

use quinn_proto::crypto::rustls::{QuicClientConfig, QuicServerConfig};

pub use quinn_proto::crypto::{
    rustls::NoInitialCipherSuite, ClientConfig, ExportKeyingMaterialError, HandshakeTokenKey,
    HmacKey, ServerConfig,
};

pub trait CryptoConfigExt: Sized {
    type RustlsConfig;

    fn with_rustls_config(config: Self::RustlsConfig) -> Result<Self, NoInitialCipherSuite>;
}

impl CryptoConfigExt for quinn_proto::ClientConfig {
    type RustlsConfig = rustls::ClientConfig;

    fn with_rustls_config(config: Self::RustlsConfig) -> Result<Self, NoInitialCipherSuite> {
        config
            .try_into()
            .map(|config: QuicClientConfig| Self::new(Arc::new(config)))
    }
}

impl CryptoConfigExt for quinn_proto::ServerConfig {
    type RustlsConfig = rustls::ServerConfig;

    fn with_rustls_config(config: Self::RustlsConfig) -> Result<Self, NoInitialCipherSuite> {
        config
            .try_into()
            .map(|config: QuicServerConfig| Self::with_crypto(Arc::new(config)))
    }
}
