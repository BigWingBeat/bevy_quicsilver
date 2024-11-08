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

use ::rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
    DigitallySignedStruct, SignatureScheme,
};

mod tofu;

pub use tofu::*;

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
