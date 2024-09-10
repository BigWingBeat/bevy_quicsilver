use std::sync::Arc;

use bevy_quicsilver::crypto::CryptoConfigExt;
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{verify_tls12_signature, verify_tls13_signature, WebPkiSupportedAlgorithms},
    pki_types::{CertificateDer, ServerName, UnixTime},
    DigitallySignedStruct, SignatureScheme,
};

#[derive(Debug)]
struct TrustOnFirstUse {
    supported_schemes: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for TrustOnFirstUse {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        todo!()
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.supported_schemes)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.supported_schemes)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.supported_schemes.supported_schemes()
    }
}

pub fn trust_on_first_use_config() -> quinn_proto::ClientConfig {
    let supported_schemes =
        rustls::crypto::ring::default_provider().signature_verification_algorithms;
    quinn_proto::ClientConfig::with_rustls_config(
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(TrustOnFirstUse { supported_schemes }))
            .with_no_client_auth(),
    )
    .unwrap()
}
