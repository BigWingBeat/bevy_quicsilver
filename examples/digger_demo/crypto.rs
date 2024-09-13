use std::{
    fs::DirEntry,
    io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use bevy_quicsilver::crypto::CryptoConfigExt;
use hashbrown::HashMap;
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{verify_tls12_signature, verify_tls13_signature, WebPkiSupportedAlgorithms},
    pki_types::{CertificateDer, ServerName, UnixTime},
    CertificateError, DigitallySignedStruct, SignatureScheme,
};

const CERT_HASH_LEN: usize = ring::digest::SHA256_OUTPUT_LEN;

enum TofuCertState {
    Good,
    Bad,
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

#[derive(Debug)]
struct TrustOnFirstUse {
    supported_schemes: WebPkiSupportedAlgorithms,
    cert_dir: PathBuf,
    cert_store: TofuCertStore,
}

impl ServerCertVerifier for TrustOnFirstUse {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
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

    let dirs = directories::ProjectDirs::from("org", "bevy_quicsilver", "bevy_quicsilver examples")
        .unwrap();

    let path = dirs.data_local_dir();
    let cert_dir = path.join("tofu");
    let cert_store = TofuCertStore::from_directory(&cert_dir).unwrap_or_default();

    quinn_proto::ClientConfig::with_rustls_config(
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(TrustOnFirstUse {
                supported_schemes,
                cert_dir,
                cert_store,
            }))
            .with_no_client_auth(),
    )
    .unwrap()
}
