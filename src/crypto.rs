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
    fs::DirEntry,
    io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use quinn_proto::crypto::rustls::{NoInitialCipherSuite, QuicClientConfig, QuicServerConfig};

pub use quinn_proto::crypto::*;

use ::rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{verify_tls12_signature, verify_tls13_signature, WebPkiSupportedAlgorithms},
    pki_types::{CertificateDer, ServerName, UnixTime},
    server::ParsedCertificate,
    CertificateError, DigitallySignedStruct, RootCertStore, SignatureScheme,
};
use hashbrown::HashMap;

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

const CERT_HASH_LEN: usize = ring::digest::SHA256_OUTPUT_LEN;

/// Certificate verification result by the trust-on-first-use certificate store
enum TofuCertState {
    /// This server has been seen before and the certificate is still the same
    Good,
    /// This server has been seen before but the certificate is different
    Bad,
    /// This server has not been seen before
    New,
}

/// Stores previously seen server certificates as a mapping between the server name and the SHA256 hash of their certificate
#[derive(Debug, Default)]
struct TofuCertStore(RwLock<HashMap<ServerName<'static>, [u8; CERT_HASH_LEN]>>);

impl TofuCertStore {
    fn from_directory(dir: &Path) -> io::Result<Self> {
        fn parse_entry(
            entry: Result<DirEntry, std::io::Error>,
        ) -> Option<(ServerName<'static>, [u8; CERT_HASH_LEN])> {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            // IPv6 addresses have their : replaced with £ so they're valid filenames on windows
            let name = name.replace('£', ":").try_into().ok()?;
            let contents = std::fs::read(entry.path()).ok()?;
            let contents = contents.try_into().ok()?;
            Some((name, contents))
        }

        let mut map = HashMap::new();
        for entry in std::fs::read_dir(dir)? {
            let Some((name, contents)) = parse_entry(entry) else {
                continue;
            };

            map.insert(name, contents);
        }
        Ok(Self(RwLock::new(map)))
    }

    fn verify(&self, cert: &CertificateDer, server_name: &ServerName<'_>) -> TofuCertState {
        let map = self.0.read().unwrap();
        let Some(known_hash) = map.get(server_name) else {
            return TofuCertState::New;
        };

        let new_hash = ring::digest::digest(&ring::digest::SHA256, cert);

        if known_hash == new_hash.as_ref() {
            TofuCertState::Good
        } else {
            TofuCertState::Bad
        }
    }

    fn store(&self, cert_dir: &Path, cert: &CertificateDer, server_name: &ServerName<'_>) {
        let hash = ring::digest::digest(&ring::digest::SHA256, cert);

        let mut map = self.0.write().unwrap();
        map.insert(server_name.to_owned(), hash.as_ref().try_into().unwrap());
        drop(map);

        if std::fs::create_dir_all(cert_dir).is_err() {
            return;
        }

        // Writing the server name as a filename is safe because it is either:
        // - An IP address, which is just numbers and .: (The : are replaced with £ so it's a valid filename on Windows)
        // - A valid DNS string, which is just ASCII letters, numbers and _-.
        let cert_file = cert_dir.join(server_name.to_str().replace(':', "£"));

        let _ = std::fs::write(cert_file, hash);
    }
}

/// A custom server certificate verifier for self-signed certificates, that implements trust-on-first-use verification.
/// This verifier checks that:
/// - The certificate is self-signed
/// - The certificate is not expired
/// - The certificate is valid for the given server name
/// - The certificate has not changed since the last connection to this server name
///
/// This verifier does NOT check revocation status due to upstream API limitations.
///
/// Self-signed certificates are distinct from certificates signed by a trusted CA (Certificate Authority).
/// The former can be generated offline and on-demand by any computer, whereas the latter serve as proof that the assertions
/// made by the certificate are accurate and trustworthy.
///
/// As a result, self-signed certificates do not tell you anything about the server, and are just a unique identifier.
/// Trust-on-first-use verification works by associating the certificate with the domain name/IP address that was used to
/// connect to the server (i.e. the server name), and comparing it on each subsequent connection.
/// If the certificate is the same, that guarantees it's the same server. Otherwise, you may have connected to a different server.
/// Note however that this guarantee no longer applies if the server's certificate gets stolen.
#[derive(Debug)]
pub struct SelfSignedTofuServerVerifier {
    supported_algs: WebPkiSupportedAlgorithms,
    cert_dir: PathBuf,
    cert_store: TofuCertStore,
}

impl ServerCertVerifier for SelfSignedTofuServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let parsed = ParsedCertificate::try_from(end_entity)?;

        // Can't support revocation checking because rustls has no public API for it.
        // webpki does, but rustls also has no public API for converting webpki errors to rustls errors

        let mut roots = RootCertStore::empty();
        // This clone shouldn't be needed, the webpki API this wraps only borrows instead of taking ownership.
        // The webpki API can't be used directly because, again, rustls has no public API for converting the error types
        roots.add(end_entity.clone())?;

        // We check that the cert is self-signed by verifying it with itself as the only trust anchor.
        // The webpki API used to convert it to a trust anchor specificially says not to do this,
        // but it works and is the only reasonable solution so we do it anyway
        ::rustls::client::verify_server_cert_signed_by_trust_anchor(
            &parsed,
            &roots,
            intermediates,
            now,
            self.supported_algs.all,
        )?;

        ::rustls::client::verify_server_name(&parsed, server_name)?;

        match self.cert_store.verify(end_entity, server_name) {
            TofuCertState::Good => Ok(ServerCertVerified::assertion()),
            TofuCertState::Bad => Err(CertificateError::ApplicationVerificationFailure.into()),
            TofuCertState::New => {
                self.cert_store
                    .store(&self.cert_dir, end_entity, server_name);
                Ok(ServerCertVerified::assertion())
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.supported_algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.supported_algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.supported_algs.supported_schemes()
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
        match self.default.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            ok @ Ok(_) => ok,
            Err(_) => self.fallback.verify_server_cert(
                end_entity,
                intermediates,
                server_name,
                ocsp_response,
                now,
            ),
        }
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

pub fn trust_on_first_use_config(
    cert_dir: PathBuf,
    // verifier: WebPkiServerVerifier,
) -> quinn_proto::ClientConfig {
    let supported_schemes =
        ::rustls::crypto::ring::default_provider().signature_verification_algorithms;

    let cert_store = TofuCertStore::from_directory(&cert_dir).unwrap_or_default();

    // quinn_proto::ClientConfig::with_rustls_config(
    //     ::rustls::ClientConfig::builder()
    //         .dangerous()
    //         .with_custom_certificate_verifier(Arc::new(TrustOnFirstUse {
    //             verifier,
    //             supported_schemes,
    //             cert_dir,
    //             cert_store,
    //         }))
    //         .with_no_client_auth(),
    // )
    // .unwrap()

    quinn_proto::ClientConfig::with_platform_verifier()
}
