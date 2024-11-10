use std::{
    fmt::Debug,
    fs::{DirEntry, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub use quinn_proto::crypto::*;

use ::rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{
        verify_tls12_signature, verify_tls13_signature, CryptoProvider, WebPkiSupportedAlgorithms,
    },
    pki_types::{CertificateDer, ServerName, UnixTime},
    server::ParsedCertificate,
    CertificateError, DigitallySignedStruct, OtherError, RootCertStore, SignatureScheme,
};
use hashbrown::HashMap;
use x509_parser::{
    error::X509Error,
    prelude::{FromDer, Validity, X509Certificate},
    time::ASN1Time,
};

pub const CERT_DIGEST_LEN: usize = ring::digest::SHA256_OUTPUT_LEN;

pub type CertDigest = [u8; CERT_DIGEST_LEN];

fn digest(end_entity: &[u8]) -> CertDigest {
    ring::digest::digest(&ring::digest::SHA256, end_entity)
        .as_ref()
        .try_into()
        .unwrap()
}

pub type TofuCertMap = HashMap<ServerName<'static>, (CertDigest, Validity)>;

/// Handles storing and retrieving server certificate information for trust-on-first-use verification
pub trait TofuServerCertStore: Debug {
    /// Write the given certificate digest and validity information to the store,
    /// using the specified server name as the key.
    fn store_certificate(
        &mut self,
        digest: CertDigest,
        validity: &Validity,
        server_name: &ServerName<'_>,
    );

    /// Retrieve the certificate digest and validity information associated with the specified server name
    /// from the store, if present.
    fn retrieve_certificate<'a, 'b: 'a>(
        &'a self,
        server_name: &ServerName<'b>,
    ) -> Option<&'a (CertDigest, Validity)>;
}

/// Implements trust-on-first-use verification by storing previously seen certificate digests in memory
#[derive(Debug, Default)]
pub struct InMemoryTofuServerCertStore {
    map: TofuCertMap,
}

impl InMemoryTofuServerCertStore {
    /// Creates a new, empty in-memory cert store
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a new cert store populated with the given server name -> cert digest mappings
    pub fn new_with_certs(certs: TofuCertMap) -> Self {
        Self { map: certs }
    }
}

impl TofuServerCertStore for InMemoryTofuServerCertStore {
    fn store_certificate(
        &mut self,
        digest: CertDigest,
        validity: &Validity,
        server_name: &ServerName<'_>,
    ) {
        self.map
            .insert(server_name.to_owned(), (digest, validity.clone()));
    }

    fn retrieve_certificate<'a, 'b: 'a>(
        &'a self,
        server_name: &ServerName<'b>,
    ) -> Option<&'a (CertDigest, Validity)> {
        self.map.get(server_name)
    }
}

/// Implements trust-on-first-use verification by writing certificate digests to the filesystem.
#[derive(Debug)]
pub struct FilesystemTofuServerCertStore {
    fs_path: PathBuf,
    map: TofuCertMap,
}

impl FilesystemTofuServerCertStore {
    /// Creates a new filesystem cert store, that will store certificate digests in the specified directory,
    /// and initially populate it by reading the contents of that directory.
    pub fn new(path: impl Into<PathBuf>) -> io::Result<Self> {
        let fs_path = path.into();
        let mut map = TofuCertMap::new();
        Self::parse_directory(&fs_path, &mut map)?;
        Ok(Self { fs_path, map })
    }

    /// Creates a new filesystem cert store, that will store certificate digests in the specified directory,
    /// and initially populate it by reading the contents of that directory,
    /// as well as with the given server name -> cert digest mappings.
    pub fn new_with_certs(path: impl Into<PathBuf>, mut certs: TofuCertMap) -> io::Result<Self> {
        let fs_path = path.into();
        Self::parse_directory(&fs_path, &mut certs)?;
        Ok(Self {
            fs_path,
            map: certs,
        })
    }

    /// Creates a new filesystem cert store, that will store certificate digests in the specified directory,
    /// but *do not* initially populate it with anything. Any certificate digests already present in the given directory
    /// will be ignored and potentially overwritten.
    pub fn new_empty(path: impl Into<PathBuf>) -> Self {
        Self {
            fs_path: path.into(),
            map: TofuCertMap::default(),
        }
    }

    /// Creates a new filesystem cert store, that will store certificate digests in the specified directory,
    /// and initially populate it with the given server name -> cert digest mappings.
    /// Any certificate digests already present in the given directory will be ignored and potentially overwritten.
    pub fn new_empty_with_certs(path: impl Into<PathBuf>, certs: TofuCertMap) -> Self {
        Self {
            fs_path: path.into(),
            map: certs,
        }
    }

    fn parse_directory(dir: &Path, map: &mut TofuCertMap) -> io::Result<()> {
        fn parse_entry(
            entry: Result<DirEntry, std::io::Error>,
        ) -> Option<(ServerName<'static>, CertDigest, Validity)> {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            // IPv6 addresses have their : replaced with £ so they're valid filenames on windows
            let name = name.replace('£', ":").try_into().ok()?;

            // Read the SHA256 digest from the file
            let mut file = File::open(entry.path()).ok()?;
            let mut digest = [0; CERT_DIGEST_LEN];
            file.read_exact(&mut digest).ok()?;

            // Read the validity information from the file
            let mut not_before = [0; 8];
            file.read_exact(&mut not_before).ok()?;

            let mut not_after = [0; 8];
            file.read_exact(&mut not_after).ok()?;

            let validity = Validity {
                not_before: ASN1Time::from_timestamp(i64::from_le_bytes(not_before)).ok()?,
                not_after: ASN1Time::from_timestamp(i64::from_le_bytes(not_after)).ok()?,
            };

            // Read 1 more byte to check if there's any data left in the file, without reading the whole thing into memory.
            // UnexpectedEof means the data we read previously was the entire contents of the file, which is what we want.
            // If there's more data, that means the file is not one of our tofu store entries, so we skip it
            file.read_exact(&mut [0])
                .is_err_and(|e| e.kind() == std::io::ErrorKind::UnexpectedEof)
                .then_some((name, digest, validity))
        }

        std::fs::create_dir_all(dir)?;
        for entry in std::fs::read_dir(dir)? {
            let Some((name, digest, validity)) = parse_entry(entry) else {
                continue;
            };

            map.insert(name, (digest, validity));
        }
        Ok(())
    }
}

impl TofuServerCertStore for FilesystemTofuServerCertStore {
    fn store_certificate(
        &mut self,
        digest: CertDigest,
        validity: &Validity,
        server_name: &ServerName<'_>,
    ) {
        self.map
            .insert(server_name.to_owned(), (digest, validity.clone()));

        if std::fs::create_dir_all(&self.fs_path).is_err() {
            // I/O error means this digest isn't persisted to disk.
            // Not fatal because it's still stored in memory,
            // so it will still work until the program is restarted
            return;
        }

        // Writing the server name as a filename is safe because it is either:
        // - An IP address, which is just numbers and .: (The : are replaced with £ so it's a valid filename on Windows)
        // - A valid DNS string, which is just ASCII letters, numbers and _-.
        let cert_file = self.fs_path.join(server_name.to_str().replace(':', "£"));

        let Ok(mut file) = std::fs::File::create(cert_file) else {
            // Ignore result for same reason as above, I/O error is not fatal
            return;
        };

        let not_before = validity.not_before.timestamp().to_le_bytes();
        let not_after = validity.not_after.timestamp().to_le_bytes();

        // If some of these writes fail then the file will be missing some data.
        // This is fine because the parser only accepts files with the exact right amount of data,
        // so the end result will be the same as the previous early returns
        let _ = file.write_all(&digest);
        let _ = file.write_all(&not_before);
        let _ = file.write_all(&not_after);

        // let mut buf = [0; CERT_DIGEST_LEN + 16];

        // let (digest_buf, validity_buf) = buf.split_at_mut(CERT_DIGEST_LEN);
        // digest_buf.copy_from_slice(&digest);

        // let (not_before_buf, not_after_buf) = validity_buf.split_at_mut(8);
        // not_before_buf.copy_from_slice(&not_before);
        // not_after_buf.copy_from_slice(&not_after);

        // let _ = file.write_all(&buf);
    }

    fn retrieve_certificate<'a, 'b: 'a>(
        &'a self,
        server_name: &ServerName<'b>,
    ) -> Option<&'a (CertDigest, Validity)> {
        self.map.get(server_name)
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
    tofu_store: Arc<Mutex<dyn TofuServerCertStore + Send + Sync>>,
}

impl SelfSignedTofuServerVerifier {
    /// Create a new self-signed server certificate verifier, using the specified trust-on-first-use store implementation,
    /// and the [process-default `CryptoProvider`][CryptoProvider#using-the-per-process-default-cryptoprovider].
    pub fn new(tofu_store: Arc<Mutex<dyn TofuServerCertStore + Send + Sync>>) -> Self {
        Self {
            supported_algs: ::rustls::crypto::CryptoProvider::get_default()
                .expect("no process-level CryptoProvider available -- call CryptoProvider::install_default() before this point")
                .signature_verification_algorithms,
            tofu_store,
        }
    }

    /// Create a new self-signed server certificate verifier, using the specified trust-on-first-use store implementation,
    /// and the specified crypto provider.
    pub fn new_with_provider(
        tofu_verifier: Arc<Mutex<dyn TofuServerCertStore + Send + Sync>>,
        provider: Arc<CryptoProvider>,
    ) -> Self {
        Self {
            supported_algs: provider.signature_verification_algorithms,
            tofu_store: tofu_verifier,
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

        // Neither rustls or webpki have any APIs that let you actually inspect the contents of the certificate,
        // so we have to use an entirely different library here (`x509_parser`)
        // to re-parse the certificate and read the validity information
        let (remainder, parsed) = X509Certificate::from_der(end_entity)
            .map_err(|e| CertificateError::Other(OtherError(Arc::new(e))))?;

        if !remainder.is_empty() {
            // Something went wrong with parsing
            return Err(CertificateError::ApplicationVerificationFailure.into());
        }

        let digest = digest(end_entity);
        let validity = parsed.validity();

        let mut tofu_store = self.tofu_store.lock().unwrap();

        let Some((old_digest, old_validity)) = tofu_store.retrieve_certificate(server_name) else {
            // New server name that was not present in the store
            // (This part is why this verification scheme is called 'trust-on-first-use')
            tofu_store.store_certificate(digest, validity, server_name);
            return Ok(ServerCertVerified::assertion());
        };

        // Convert from the rustls time type to the x509_parser time type
        let now = now
            .as_secs()
            .try_into()
            .map_err(|_| X509Error::InvalidDate)
            .and_then(ASN1Time::from_timestamp)
            .map_err(|e| CertificateError::Other(OtherError(Arc::new(e))))?;

        // Check the validity of the stored certificate, so that servers can configure their certs to expire and renew,
        // without our tofu store causing issues by holding onto an old expired cert forever
        if old_validity.is_valid_at(now) {
            // Stored certificate is still valid, compare the digests
            if *old_digest == digest {
                Ok(ServerCertVerified::assertion())
            } else {
                Err(rustls::Error::InvalidCertificate(
                    CertificateError::ApplicationVerificationFailure,
                ))
            }
        } else {
            // Stored certificate is no longer valid, so replace it with the new certificate
            // because we know the new one is currently valid
            tofu_store.store_certificate(digest, validity, server_name);
            Ok(ServerCertVerified::assertion())
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
