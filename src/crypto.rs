use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::WebPkiSupportedAlgorithms,
    pki_types::{CertificateDer, ServerName, UnixTime},
    DigitallySignedStruct, SignatureScheme,
};

/// A dummy [`ServerCertVerifier`] that treats any certificate as valid.
/// This should not be used in production, as it is vulnerable to man-in-the-middle attacks.
#[derive(Debug)]
pub struct NoServerVerification(WebPkiSupportedAlgorithms);

impl NoServerVerification {
    fn new(algorithms: WebPkiSupportedAlgorithms) -> Self {
        Self(algorithms)
    }
}

impl ServerCertVerifier for NoServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

pub mod client {
    use std::sync::Arc;

    use rustls::{client::danger::ServerCertVerifier, ClientConfig, RootCertStore};

    /// Create a [`ClientConfig`] that trusts the platform's native certificate verifier, using [rustls-platform-verifier]
    ///
    /// [rustls-platform-verifier]: https://crates.io/crates/rustls-platform-verifier
    #[cfg(feature = "rustls-platform-verifier")]
    pub fn config_with_platform_verifier() -> ClientConfig {
        rustls_platform_verifier::tls_config()
    }

    /// Create a [`ClientConfig`] that trusts the given root certificates
    pub fn config_with_root_certs(root_store: RootCertStore) -> ClientConfig {
        ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_root_certificates(root_store)
            .with_no_client_auth()
    }

    /// Create a [`ClientConfig`] with a custom certificate verifier
    pub fn config_with_cert_verifier(
        cert_verifier: impl ServerCertVerifier + 'static,
    ) -> ClientConfig {
        ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(cert_verifier))
            .with_no_client_auth()
    }
}

pub mod server {
    use std::sync::Arc;

    use rustls::{
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
        server::ResolvesServerCert,
        ServerConfig,
    };

    use crate::Error;

    /// Create a [`ServerConfig`] by generating a self-signed certificate using the specified server name(s)
    ///
    /// `subject_alt_names` must include the exact value(s) that clients will specify as the `server_name` parameter of
    /// [`Endpoint::connect`] in order for the connection to succeed.
    ///
    /// [`Endpoint::connect`]: https://docs.rs/quinn/latest/quinn/struct.Endpoint.html#method.connect
    pub fn config_with_gen_self_signed(
        subject_alt_names: impl Into<Vec<String>>,
    ) -> Result<ServerConfig, Error> {
        let certificate = rcgen::generate_simple_self_signed(subject_alt_names)?;
        config_with_single_cert(
            vec![CertificateDer::from(certificate.cert)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                certificate.key_pair.serialize_der(),
            )),
        )
        .map_err(Into::into)
    }

    /// Create a [`ServerConfig`] with the specified certificate and private key
    pub fn config_with_single_cert(
        cert_chain: Vec<CertificateDer<'static>>,
        key_der: PrivateKeyDer<'static>,
    ) -> Result<ServerConfig, rustls::Error> {
        ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_single_cert(cert_chain, key_der)
    }

    /// Create a [`ServerConfig`] with a custom certificate resolver
    pub fn config_with_cert_resolver(resolver: impl ResolvesServerCert + 'static) -> ServerConfig {
        ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(resolver))
    }
}
