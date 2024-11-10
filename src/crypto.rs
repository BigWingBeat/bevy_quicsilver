//! Traits and implementations for the QUIC cryptography protocol
//!
//! The protocol logic in Quinn is contained in types that abstract over the actual
//! cryptographic protocol used. This module contains the traits used for this
//! abstraction layer as well as a single implementation of these traits that uses
//! *ring* and rustls to implement the TLS protocol support.
//!
//! Note that usage of any protocol (version) other than TLS 1.3 does not conform to any
//! published versions of the specification, and will not be supported in QUIC v1.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use quinn_proto::crypto::rustls::{NoInitialCipherSuite, QuicClientConfig, QuicServerConfig};

pub use quinn_proto::crypto::*;

use ::rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
    DigitallySignedStruct, SignatureScheme,
};

mod tofu;

pub use tofu::*;

/// Extension trait for helper methods to provide different server certificate verifiers.
pub trait ClientConfigExt: Sized {
    /// Create a client config from a custom rustls config.
    fn with_rustls_config(config: ::rustls::ClientConfig) -> Result<Self, NoInitialCipherSuite>;

    /// Create a client config with a trust-on-first-use verifier for self-signed certificates,
    /// that writes certificate digests to the filesystem in the specified directory.
    fn with_trust_on_first_use(path: impl Into<PathBuf>) -> Result<Self, rustls::Error>;

    /// Create a client config with a trust-on-first-use verifier for self-signed certificates,
    /// that is backed by the specified certificate store.
    fn with_custom_trust_on_first_use(
        tofu: impl TofuServerCertStore + Send + Sync + 'static,
    ) -> Result<Self, NoInitialCipherSuite>;

    /// Create a client config with two certificate verifiers, that verifies with `default` first,
    /// and if that returns an error, verifies with `fallback` instead.
    fn with_fallback_verifier(
        default: impl ServerCertVerifier + 'static,
        fallback: impl ServerCertVerifier + 'static,
    ) -> Result<Self, NoInitialCipherSuite>;
}

impl ClientConfigExt for quinn_proto::ClientConfig {
    fn with_rustls_config(config: ::rustls::ClientConfig) -> Result<Self, NoInitialCipherSuite> {
        config
            .try_into()
            .map(|config: QuicClientConfig| Self::new(Arc::new(config)))
    }

    fn with_trust_on_first_use(path: impl Into<PathBuf>) -> Result<Self, rustls::Error> {
        Self::with_rustls_config(
            ::rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(SelfSignedTofuServerVerifier::new(
                    Arc::new(Mutex::new(
                        FilesystemTofuServerCertStore::new(path)
                            .map_err(|e| ::rustls::OtherError(Arc::new(e)))?,
                    )),
                )))
                .with_no_client_auth(),
        )
        .map_err(|e| ::rustls::OtherError(Arc::new(e)).into())
    }

    fn with_custom_trust_on_first_use(
        tofu: impl TofuServerCertStore + Send + Sync + 'static,
    ) -> Result<Self, NoInitialCipherSuite> {
        Self::with_rustls_config(
            ::rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(SelfSignedTofuServerVerifier::new(
                    Arc::new(Mutex::new(tofu)),
                )))
                .with_no_client_auth(),
        )
    }

    fn with_fallback_verifier(
        default: impl ServerCertVerifier + 'static,
        fallback: impl ServerCertVerifier + 'static,
    ) -> Result<Self, NoInitialCipherSuite> {
        Self::with_rustls_config(
            ::rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(FallbackVerifier { default, fallback }))
                .with_no_client_auth(),
        )
    }
}

/// Extension trait to make it easier to provide a custom crypto config.
pub trait ServerConfigExt: Sized {
    type RustlsConfig;

    /// Create a server config from a custom rustls config.
    fn with_rustls_config(config: Self::RustlsConfig) -> Result<Self, NoInitialCipherSuite>;
}

impl ServerConfigExt for quinn_proto::ServerConfig {
    type RustlsConfig = ::rustls::ServerConfig;

    fn with_rustls_config(config: Self::RustlsConfig) -> Result<Self, NoInitialCipherSuite> {
        config
            .try_into()
            .map(|config: QuicServerConfig| Self::with_crypto(Arc::new(config)))
    }
}

/// A ServerCertVerifier that wraps two other verifiers.
/// It attempts to verify with `default`, and if that returns an error, it verifies with `fallback` instead.
#[derive(Debug)]
pub struct FallbackVerifier<T, F> {
    pub default: T,
    pub fallback: F,
}

impl<T: ServerCertVerifier, F: ServerCertVerifier> ServerCertVerifier for FallbackVerifier<T, F> {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.default
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
            .or_else(|_| {
                self.fallback.verify_server_cert(
                    end_entity,
                    intermediates,
                    server_name,
                    ocsp_response,
                    now,
                )
            })
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.default.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.default.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.default.supported_verify_schemes()
    }
}
