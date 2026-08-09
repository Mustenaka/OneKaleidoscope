use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand_core::{OsRng, RngCore};
use rcgen::generate_simple_self_signed;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{
    CertificateError, ClientConfig, ClientConnection, DigitallySignedStruct, Error as RustlsError,
    ServerConfig, ServerConnection, SignatureScheme,
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use x509_cert::der::{Decode, Encode};
use x509_cert::Certificate;
use zeroize::{Zeroize, Zeroizing};

use crate::error::TransportError;
use crate::platform;

pub const DEVICE_AUTH_EXPORTER_LABEL: &[u8] = b"EXPORTER-OneKaleidoscope-R3-DeviceAuth";

#[derive(Clone, PartialEq, Eq)]
pub struct SpkiPin([u8; 32]);

impl std::fmt::Debug for SpkiPin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SpkiPin([redacted])")
    }
}

impl SpkiPin {
    pub fn parse(encoded: &str) -> Result<Self, TransportError> {
        let digest = encoded
            .strip_prefix("sha256:")
            .ok_or(TransportError::InvalidKeyMaterial)?;
        if digest.len() != 43 || digest.contains('=') {
            return Err(TransportError::InvalidKeyMaterial);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(digest)
            .map_err(|_| TransportError::InvalidKeyMaterial)?;
        let pin: [u8; 32] = bytes
            .try_into()
            .map_err(|_| TransportError::InvalidKeyMaterial)?;
        if URL_SAFE_NO_PAD.encode(pin) != digest {
            return Err(TransportError::InvalidKeyMaterial);
        }
        Ok(Self(pin))
    }

    pub fn from_certificate_der(certificate_der: &[u8]) -> Result<Self, TransportError> {
        let spki = extract_spki_der(certificate_der)?;
        Ok(Self(Sha256::digest(&spki).into()))
    }

    pub fn encode(&self) -> String {
        format!("sha256:{}", URL_SAFE_NO_PAD.encode(self.0))
    }

    pub fn verify_certificate_der(&self, certificate_der: &[u8]) -> Result<(), TransportError> {
        let actual = Self::from_certificate_der(certificate_der)?;
        if self.0.ct_eq(&actual.0).into() {
            Ok(())
        } else {
            Err(TransportError::AuthenticationFailed)
        }
    }
}

#[derive(Clone)]
pub struct TlsIdentity {
    certificate_chain: Vec<CertificateDer<'static>>,
    private_key_pkcs8: Vec<u8>,
}

impl Drop for TlsIdentity {
    fn drop(&mut self) {
        self.private_key_pkcs8.zeroize();
    }
}

impl std::fmt::Debug for TlsIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TlsIdentity")
            .field("certificate_count", &self.certificate_chain.len())
            .field("private_key_pkcs8", &"[redacted]")
            .finish()
    }
}

impl TlsIdentity {
    pub fn from_pkcs8_der(
        certificate_chain: Vec<Vec<u8>>,
        private_key_pkcs8: Vec<u8>,
    ) -> Result<Self, TransportError> {
        if certificate_chain.is_empty() || private_key_pkcs8.is_empty() {
            return Err(TransportError::InvalidKeyMaterial);
        }
        for certificate in &certificate_chain {
            extract_spki_der(certificate)?;
        }
        Ok(Self {
            certificate_chain: certificate_chain
                .into_iter()
                .map(CertificateDer::from)
                .collect(),
            private_key_pkcs8,
        })
    }

    pub fn leaf_pin(&self) -> Result<SpkiPin, TransportError> {
        let leaf = self
            .certificate_chain
            .first()
            .ok_or(TransportError::InvalidKeyMaterial)?;
        SpkiPin::from_certificate_der(leaf.as_ref())
    }
}

pub fn server_config(identity: TlsIdentity) -> Result<Arc<ServerConfig>, TransportError> {
    let provider = Arc::new(ring::default_provider());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.private_key_pkcs8.clone()));
    let config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| TransportError::InvalidKeyMaterial)?
        .with_no_client_auth()
        .with_single_cert(identity.certificate_chain.clone(), key)
        .map_err(|_| TransportError::InvalidKeyMaterial)?;
    Ok(Arc::new(config))
}

pub struct TlsIdentityStore {
    path: PathBuf,
}

impl std::fmt::Debug for TlsIdentityStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TlsIdentityStore([redacted path])")
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredIdentity {
    #[serde(rename = "version")]
    revision: u64,
    certificate_chain: Vec<String>,
    private_key_pkcs8: String,
}

impl TlsIdentityStore {
    pub fn new(path: PathBuf) -> Result<Self, TransportError> {
        let parent = path.parent().ok_or(TransportError::InsecurePermissions)?;
        platform::prepare_private_directory(parent)
            .map_err(|_| TransportError::InsecurePermissions)?;
        if path.exists() {
            platform::verify_private_path(&path)
                .map_err(|_| TransportError::InsecurePermissions)?;
        }
        Ok(Self { path })
    }

    pub fn load_or_generate(&self) -> Result<TlsIdentity, TransportError> {
        if self.path.exists() {
            self.load()
        } else {
            self.generate_and_store()
        }
    }

    pub fn load(&self) -> Result<TlsIdentity, TransportError> {
        platform::verify_private_path(&self.path)
            .map_err(|_| TransportError::InsecurePermissions)?;
        let metadata = fs::metadata(&self.path).map_err(TransportError::from)?;
        if metadata.len() == 0 || metadata.len() > 65_536 {
            return Err(TransportError::InvalidKeyMaterial);
        }
        let capacity =
            usize::try_from(metadata.len()).map_err(|_| TransportError::InvalidKeyMaterial)?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(capacity));
        fs::File::open(&self.path)
            .and_then(|mut file| file.read_to_end(&mut bytes))
            .map_err(TransportError::from)?;
        let stored: StoredIdentity =
            serde_json::from_slice(&bytes).map_err(|_| TransportError::InvalidKeyMaterial)?;
        if stored.revision != 1 || stored.certificate_chain.is_empty() {
            return Err(TransportError::InvalidKeyMaterial);
        }
        let certificates = stored
            .certificate_chain
            .iter()
            .map(|encoded| decode_canonical_base64(encoded))
            .collect::<Result<Vec<_>, _>>()?;
        let key = Zeroizing::new(decode_canonical_base64(&stored.private_key_pkcs8)?);
        let identity = TlsIdentity::from_pkcs8_der(certificates, key.to_vec())?;
        server_config(identity.clone())?;
        Ok(identity)
    }

    fn generate_and_store(&self) -> Result<TlsIdentity, TransportError> {
        let generated = generate_simple_self_signed(vec!["onekaleidoscope.invalid".to_owned()])
            .map_err(|_| TransportError::InvalidKeyMaterial)?;
        let identity = TlsIdentity::from_pkcs8_der(
            vec![generated.cert.der().to_vec()],
            generated.key_pair.serialize_der(),
        )?;
        server_config(identity.clone())?;
        let stored = StoredIdentity {
            revision: 1,
            certificate_chain: identity
                .certificate_chain
                .iter()
                .map(|certificate| URL_SAFE_NO_PAD.encode(certificate.as_ref()))
                .collect(),
            private_key_pkcs8: URL_SAFE_NO_PAD.encode(&identity.private_key_pkcs8),
        };
        let bytes = Zeroizing::new(
            serde_json::to_vec(&stored).map_err(|_| TransportError::InvalidKeyMaterial)?,
        );
        write_private_file_atomic(&self.path, &bytes)?;
        Ok(identity)
    }
}

fn decode_canonical_base64(encoded: &str) -> Result<Vec<u8>, TransportError> {
    if encoded.is_empty() || encoded.contains('=') {
        return Err(TransportError::InvalidKeyMaterial);
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| TransportError::InvalidKeyMaterial)?;
    if URL_SAFE_NO_PAD.encode(&decoded) != encoded {
        return Err(TransportError::InvalidKeyMaterial);
    }
    Ok(decoded)
}

fn write_private_file_atomic(path: &Path, bytes: &[u8]) -> Result<(), TransportError> {
    let parent = path.parent().ok_or(TransportError::InsecurePermissions)?;
    platform::prepare_private_directory(parent).map_err(|_| TransportError::InsecurePermissions)?;
    if path.exists() {
        platform::verify_private_path(path).map_err(|_| TransportError::InsecurePermissions)?;
    }
    let mut random = [0_u8; 16];
    OsRng.fill_bytes(&mut random);
    let temporary = parent.join(format!(".tls-{}.tmp", URL_SAFE_NO_PAD.encode(random)));
    let mut file = platform::secure_private_file(&temporary, true).map_err(TransportError::from)?;
    let result = (|| {
        file.write_all(bytes).map_err(TransportError::from)?;
        file.sync_all().map_err(TransportError::from)?;
        drop(file);
        platform::atomic_replace(&temporary, path).map_err(TransportError::from)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn client_config(pin: SpkiPin) -> Result<Arc<ClientConfig>, TransportError> {
    let provider = Arc::new(ring::default_provider());
    let verifier = Arc::new(ExactSpkiVerifier {
        pin,
        provider: provider.clone(),
    });
    let config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| TransportError::InvalidKeyMaterial)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

pub fn export_server_device_auth_binding(
    connection: &ServerConnection,
) -> Result<[u8; 32], TransportError> {
    let mut output = [0_u8; 32];
    connection
        .export_keying_material(&mut output, DEVICE_AUTH_EXPORTER_LABEL, None)
        .map_err(|_| TransportError::AuthenticationFailed)?;
    Ok(output)
}

pub fn export_client_device_auth_binding(
    connection: &ClientConnection,
) -> Result<[u8; 32], TransportError> {
    let mut output = [0_u8; 32];
    connection
        .export_keying_material(&mut output, DEVICE_AUTH_EXPORTER_LABEL, None)
        .map_err(|_| TransportError::AuthenticationFailed)?;
    Ok(output)
}

fn extract_spki_der(certificate_der: &[u8]) -> Result<Vec<u8>, TransportError> {
    let certificate =
        Certificate::from_der(certificate_der).map_err(|_| TransportError::InvalidKeyMaterial)?;
    let canonical_certificate = certificate
        .to_der()
        .map_err(|_| TransportError::InvalidKeyMaterial)?;
    if canonical_certificate != certificate_der {
        return Err(TransportError::InvalidKeyMaterial);
    }
    certificate
        .tbs_certificate
        .subject_public_key_info
        .to_der()
        .map_err(|_| TransportError::InvalidKeyMaterial)
}

#[derive(Debug)]
struct ExactSpkiVerifier {
    pin: SpkiPin,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for ExactSpkiVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        self.pin
            .verify_certificate_der(end_entity.as_ref())
            .map_err(|error| match error {
                TransportError::InvalidKeyMaterial => {
                    RustlsError::InvalidCertificate(CertificateError::BadEncoding)
                }
                _ => RustlsError::InvalidCertificate(
                    CertificateError::ApplicationVerificationFailure,
                ),
            })?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        verify_tls12_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        verify_tls13_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::io::Cursor;

    use base64::Engine;
    use rcgen::generate_simple_self_signed;
    use rustls::pki_types::ServerName;
    use rustls::{ClientConnection, ProtocolVersion, ServerConnection};
    use sha2::Digest;

    use super::{
        client_config, export_client_device_auth_binding, export_server_device_auth_binding,
        server_config, SpkiPin, TlsIdentity, TlsIdentityStore,
    };

    fn identity() -> (TlsIdentity, Vec<u8>) {
        let generated = generate_simple_self_signed(vec!["localhost".to_owned()])
            .expect("generate certificate");
        let certificate = generated.cert.der().to_vec();
        let key = generated.key_pair.serialize_der();
        (
            TlsIdentity::from_pkcs8_der(vec![certificate.clone()], key).expect("identity"),
            certificate,
        )
    }

    fn handshake(client: &mut ClientConnection, server: &mut ServerConnection) -> Result<(), ()> {
        for _ in 0..16 {
            let mut client_wire = Vec::new();
            client.write_tls(&mut client_wire).map_err(|_| ())?;
            server
                .read_tls(&mut Cursor::new(client_wire))
                .map_err(|_| ())?;
            server.process_new_packets().map_err(|_| ())?;

            let mut server_wire = Vec::new();
            server.write_tls(&mut server_wire).map_err(|_| ())?;
            client
                .read_tls(&mut Cursor::new(server_wire))
                .map_err(|_| ())?;
            client.process_new_packets().map_err(|_| ())?;
            if !client.is_handshaking() && !server.is_handshaking() {
                return Ok(());
            }
        }
        Err(())
    }

    #[test]
    fn tls_is_13_only_and_exact_pin_succeeds() {
        let (identity, certificate) = identity();
        let pin = SpkiPin::from_certificate_der(&certificate).expect("pin");
        let server_config = server_config(identity).expect("server config");
        let client_config = client_config(pin).expect("client config");
        assert_eq!(
            server_config.max_fragment_size, None,
            "default fragment policy changed"
        );
        let mut client = ClientConnection::new(
            client_config,
            ServerName::try_from("localhost")
                .expect("server name")
                .to_owned(),
        )
        .expect("client");
        let mut server = ServerConnection::new(server_config).expect("server");
        handshake(&mut client, &mut server).expect("TLS handshake");
        assert_eq!(client.protocol_version(), Some(ProtocolVersion::TLSv1_3));
        assert_eq!(server.protocol_version(), Some(ProtocolVersion::TLSv1_3));
        assert_eq!(
            export_client_device_auth_binding(&client).expect("client exporter"),
            export_server_device_auth_binding(&server).expect("server exporter")
        );
    }

    #[test]
    fn wrong_spki_pin_aborts_the_handshake() {
        let (identity, _) = identity();
        let wrong_pin = SpkiPin::parse("sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .expect("pin shape");
        let mut client = ClientConnection::new(
            client_config(wrong_pin).expect("client config"),
            ServerName::try_from("localhost")
                .expect("server name")
                .to_owned(),
        )
        .expect("client");
        let mut server =
            ServerConnection::new(server_config(identity).expect("server config")).expect("server");
        assert!(handshake(&mut client, &mut server).is_err());
    }

    #[test]
    fn pin_is_over_spki_not_entire_certificate() {
        let (_, certificate) = identity();
        let pin = SpkiPin::from_certificate_der(&certificate).expect("pin");
        assert_ne!(
            pin.encode(),
            format!(
                "sha256:{}",
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(sha2::Sha256::digest(&certificate))
            )
        );
        pin.verify_certificate_der(&certificate).expect("verify");
    }

    #[test]
    fn production_identity_is_generated_once_reused_and_corruption_is_fail_loud() {
        let directory = std::env::temp_dir().join(format!(
            "kaleido-transport-tls-identity-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let path = directory.join("identity.json");
        let store = TlsIdentityStore::new(path.clone()).expect("identity store");
        let first_pin = store
            .load_or_generate()
            .expect("generate")
            .leaf_pin()
            .expect("first pin");
        let second_pin = TlsIdentityStore::new(path.clone())
            .expect("reopen")
            .load_or_generate()
            .expect("load")
            .leaf_pin()
            .expect("second pin");
        assert_eq!(first_pin, second_pin);

        std::fs::write(&path, b"corrupt").expect("corrupt existing secure file");
        assert!(matches!(
            TlsIdentityStore::new(path)
                .expect("reopen corrupt")
                .load_or_generate(),
            Err(crate::error::TransportError::InvalidKeyMaterial)
        ));
        std::fs::remove_dir_all(&directory).expect("cleanup");
    }
}
