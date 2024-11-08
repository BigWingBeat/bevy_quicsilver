use std::{
    fmt::Debug,
    fs::{DirEntry, File},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

pub use quinn_proto::crypto::*;

use ::rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{
        verify_tls12_signature, verify_tls13_signature, CryptoProvider, WebPkiSupportedAlgorithms,
    },
    pki_types::{CertificateDer, ServerName, UnixTime},
    server::ParsedCertificate,
    CertificateError, DigitallySignedStruct, RootCertStore, SignatureScheme,
};
use hashbrown::HashMap;

pub const CERT_DIGEST_LEN: usize = ring::digest::SHA256_OUTPUT_LEN;

pub type TofuCertMap = HashMap<ServerName<'static>, [u8; CERT_DIGEST_LEN]>;

/// Handles storing and retrieving server certificates for trust-on-first-use verification
pub trait TofuServerCertStore: Debug {
    /// Perform trust-on-first-use verification for the given end-entity certificate and server name
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer,
        server_name: &ServerName<'_>,
    ) -> Result<ServerCertVerified, rustls::Error>;
}

/// Implements trust-on-first-use verification by storing previously seen certificate digests in memory
#[derive(Debug, Default)]
pub struct InMemoryTofuServerCertStore {
    map: RwLock<TofuCertMap>,
}

impl InMemoryTofuServerCertStore {
    /// Creates a new, empty in-memory cert store
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a new cert store populated with the given server name -> cert digest mappings
    pub fn new_with_certs(certs: TofuCertMap) -> Self {
        Self {
            map: RwLock::new(certs),
        }
    }
}

impl TofuServerCertStore for InMemoryTofuServerCertStore {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer,
        server_name: &ServerName<'_>,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let mut map = self.map.write().unwrap();
        let hash = ring::digest::digest(&ring::digest::SHA256, end_entity);

        let Some(known_hash) = map.get(server_name) else {
            // New server name
            map.insert(server_name.to_owned(), hash.as_ref().try_into().unwrap());
            return Ok(ServerCertVerified::assertion());
        };

        if known_hash == hash.as_ref() {
            // Cert matches previous entry for this server name
            Ok(ServerCertVerified::assertion())
        } else {
            // Cert does not match
            Err(rustls::Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
        }
    }
}

/// Implements trust-on-first-use verification by writing certificate digests to the filesystem.
#[derive(Debug)]
pub struct FilesystemTofuServerCertStore {
    fs_path: PathBuf,
    map: RwLock<TofuCertMap>,
}

impl FilesystemTofuServerCertStore {
    /// Creates a new filesystem cert store, that will store certificate digests in the specified directory,
    /// and initially populate it by reading the contents of that directory.
    pub fn new(path: impl Into<PathBuf>) -> io::Result<Self> {
        let fs_path = path.into();
        let mut map = TofuCertMap::new();
        Self::parse_directory(&fs_path, &mut map)?;
        Ok(Self {
            fs_path,
            map: RwLock::new(map),
        })
    }

    /// Creates a new filesystem cert store, that will store certificate digests in the specified directory,
    /// and initially populate it by reading the contents of that directory,
    /// as well as with the given server name -> cert digest mappings.
    pub fn new_with_certs(path: impl Into<PathBuf>, mut certs: TofuCertMap) -> io::Result<Self> {
        let fs_path = path.into();
        Self::parse_directory(&fs_path, &mut certs)?;
        Ok(Self {
            fs_path,
            map: RwLock::new(certs),
        })
    }

    /// Creates a new filesystem cert store, that will store certificate digests in the specified directory,
    /// but *do not* initially populate it with anything. Any certificate digests already present in the given directory
    /// will be ignored and potentially overwritten.
    pub fn new_empty(path: impl Into<PathBuf>) -> Self {
        Self {
            fs_path: path.into(),
            map: RwLock::default(),
        }
    }

    /// Creates a new filesystem cert store, that will store certificate digests in the specified directory,
    /// and initially populate it with the given server name -> cert digest mappings.
    /// Any certificate digests already present in the given directory will be ignored and potentially overwritten.
    pub fn new_empty_with_certs(path: impl Into<PathBuf>, certs: TofuCertMap) -> Self {
        Self {
            fs_path: path.into(),
            map: RwLock::new(certs),
        }
    }

    fn parse_directory(dir: &Path, map: &mut TofuCertMap) -> io::Result<()> {
        fn parse_entry(
            entry: Result<DirEntry, std::io::Error>,
        ) -> Option<(ServerName<'static>, [u8; CERT_DIGEST_LEN])> {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            // IPv6 addresses have their : replaced with £ so they're valid filenames on windows
            let name = name.replace('£', ":").try_into().ok()?;

            // Read the SHA256 hash from the file
            let mut file = File::open(entry.path()).ok()?;
            let mut hash = [0; CERT_DIGEST_LEN];
            file.read_exact(&mut hash).ok()?;

            // Read 1 more byte to check if there's any data left in the file, without reading the whole thing into memory.
            // UnexpectedEof means the hash we read previously was the entire contents of the file, which is what we want.
            // If there's more data, that means the file is not one of our certificate digests, so we skip it
            file.read_exact(&mut [0])
                .is_err_and(|e| e.kind() == std::io::ErrorKind::UnexpectedEof)
                .then_some((name, hash))
        }

        std::fs::create_dir_all(dir)?;
        for entry in std::fs::read_dir(dir)? {
            let Some((name, contents)) = parse_entry(entry) else {
                continue;
            };

            map.insert(name, contents);
        }
        Ok(())
    }

    fn store(&self, cert: &CertificateDer, server_name: &ServerName<'_>) {
        let hash = ring::digest::digest(&ring::digest::SHA256, cert);

        let mut map = self.map.write().unwrap();
        map.insert(server_name.to_owned(), hash.as_ref().try_into().unwrap());
        drop(map);

        if std::fs::create_dir_all(&self.fs_path).is_err() {
            return;
        }

        // Writing the server name as a filename is safe because it is either:
        // - An IP address, which is just numbers and .: (The : are replaced with £ so it's a valid filename on Windows)
        // - A valid DNS string, which is just ASCII letters, numbers and _-.
        let cert_file = self.fs_path.join(server_name.to_str().replace(':', "£"));

        let _ = std::fs::write(cert_file, hash);
    }
}

impl TofuServerCertStore for FilesystemTofuServerCertStore {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer,
        server_name: &ServerName<'_>,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let map = self.map.read().unwrap();
        let Some(known_hash) = map.get(server_name) else {
            // New server name
            drop(map);
            self.store(end_entity, server_name);
            return Ok(ServerCertVerified::assertion());
        };

        let new_hash = ring::digest::digest(&ring::digest::SHA256, end_entity);

        if known_hash == new_hash.as_ref() {
            // Cert matches previous entry for this server name
            Ok(ServerCertVerified::assertion())
        } else {
            // Cert does not match
            Err(rustls::Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
        }
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
    tofu_verifier: Arc<dyn TofuServerCertStore + Send + Sync>,
}

impl SelfSignedTofuServerVerifier {
    /// Create a new self-signed server certificate verifier, using the specified trust-on-first-use implementation,
    /// and the [process-default `CryptoProvider`][CryptoProvider#using-the-per-process-default-cryptoprovider].
    pub fn new(tofu_verifier: Arc<dyn TofuServerCertStore + Send + Sync>) -> Self {
        Self {
            supported_algs: ::rustls::crypto::CryptoProvider::get_default()
                .expect("no process-level CryptoProvider available -- call CryptoProvider::install_default() before this point")
                .signature_verification_algorithms,
            tofu_verifier,
        }
    }

    /// Create a new self-signed server certificate verifier, using the specified trust-on-first-use implementation,
    /// and the specified crypto provider.
    pub fn new_with_provider(
        tofu_verifier: Arc<dyn TofuServerCertStore + Send + Sync>,
        provider: Arc<CryptoProvider>,
    ) -> Self {
        Self {
            supported_algs: provider.signature_verification_algorithms,
            tofu_verifier,
        }
    }
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

        self.tofu_verifier
            .verify_server_cert(end_entity, server_name)
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
