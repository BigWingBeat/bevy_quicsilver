use std::time::SystemTime;

use rustls::{
    client::{ServerCertVerified, ServerCertVerifier},
    Certificate, ConfigBuilder, ConfigSide, ServerName, WantsCipherSuites, WantsVerifier,
};

/// A dummy [`ServerCertVerifier`] that treats any certificate as valid.
/// This should not be used in production, as it is vulnerable to man-in-the-middle attacks.
#[derive(Debug)]
pub struct NoServerVerification;

impl ServerCertVerifier for NoServerVerification {
    fn verify_server_cert(
        &self,
        _: &Certificate,
        _: &[Certificate],
        _: &ServerName,
        _: &mut dyn Iterator<Item = &[u8]>,
        _: &[u8],
        _: SystemTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
}

trait ConfigBuilderExt<S: ConfigSide> {
    fn with_quic_defaults(self) -> ConfigBuilder<S, WantsVerifier>;
}

impl<S: ConfigSide> ConfigBuilderExt<S> for ConfigBuilder<S, WantsCipherSuites> {
    /// Exactly the same as [`with_safe_defaults`] except with only TLS 1.3 enabled,
    /// rather than both TLS 1.3 & 1.2, as QUIC requires TLS 1.3
    ///
    /// [`with_safe_defaults`]: https://docs.rs/rustls/0.21.9/rustls/struct.ConfigBuilder.html#method.with_safe_defaults
    fn with_quic_defaults(self) -> ConfigBuilder<S, WantsVerifier> {
        // https://github.com/quinn-rs/quinn/blob/b5d23a864759d2d5ba819d70e30f9c87ef6784db/quinn-proto/src/crypto/rustls.rs#L383-L417
        self.with_safe_default_cipher_suites()
            .with_safe_default_kx_groups()
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
    }
}

pub mod client {
    use std::sync::Arc;

    use rustls::{client::ServerCertVerifier, ClientConfig, RootCertStore};
    use tracing::warn;

    use super::ConfigBuilderExt;

    /// Create a [`ClientConfig`] that trusts the platform's native root certificates, using [rustls-native-certs]
    ///
    /// [rustls-native-certs]: https://crates.io/crates/rustls-native-certs
    #[cfg(feature = "rustls-native-certs")]
    pub fn config_with_native_certs() -> ClientConfig {
        let mut root_store = RootCertStore::empty();
        match rustls_native_certs::load_native_certs() {
            Ok(certs) => {
                root_store.add_parsable_certificates(&certs);
                if root_store.is_empty() {
                    warn!("Could not load any platform native root certs. Enable debug-level logging for more information");
                }
            }
            Err(e) => {
                warn!("Could not load platform native root certs: {e}");
            }
        };

        config_with_root_certs(root_store)
    }

    /// Create a [`ClientConfig`] that trusts the given root certificates
    pub fn config_with_root_certs(root_store: RootCertStore) -> ClientConfig {
        ClientConfig::builder()
            .with_quic_defaults()
            .with_root_certificates(root_store)
            .with_no_client_auth()
    }

    /// Create a [`ClientConfig`] with a custom certificate verifier
    pub fn config_with_cert_verifier(
        cert_verifier: impl ServerCertVerifier + 'static,
    ) -> ClientConfig {
        ClientConfig::builder()
            .with_quic_defaults()
            .with_custom_certificate_verifier(Arc::new(cert_verifier))
            .with_no_client_auth()
    }
}

pub mod server {
    use std::sync::Arc;

    use rustls::{server::ResolvesServerCert, Certificate, PrivateKey, ServerConfig};

    use crate::Error;

    use super::ConfigBuilderExt;

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
            vec![Certificate(certificate.serialize_der()?)],
            PrivateKey(certificate.serialize_private_key_der()),
        )
        .map_err(Into::into)
    }

    /// Create a [`ServerConfig`] with the specified certificate and private key
    pub fn config_with_single_cert(
        cert_chain: Vec<Certificate>,
        key_der: PrivateKey,
    ) -> Result<ServerConfig, rustls::Error> {
        ServerConfig::builder()
            .with_quic_defaults()
            .with_no_client_auth()
            .with_single_cert(cert_chain, key_der)
    }

    /// Create a [`ServerConfig`] with a custom certificate resolver
    pub fn config_with_cert_resolver(resolver: impl ResolvesServerCert + 'static) -> ServerConfig {
        ServerConfig::builder()
            .with_quic_defaults()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(resolver))
    }
}
