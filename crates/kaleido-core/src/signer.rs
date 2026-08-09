//! Platform-keystore signing boundary for mobile device authentication.

/// Failures a platform keystore may report without exposing provider text,
/// key material or platform-specific error details across the FFI boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum DeviceSignerError {
    #[error("the device signing key is unavailable")]
    KeyUnavailable,

    #[error("the device signing key has an invalid public-key encoding")]
    InvalidPublicKey,

    #[error("the device authentication signature could not be produced")]
    SigningFailed,
}

/// The only interface through which Rust may use an Android/iOS device key.
///
/// Implementations keep the private P-256 key inside Android Keystore or the
/// iOS Keychain/Secure Enclave. Rust supplies the exact TRANSPORT transcript
/// and receives a strict DER ECDSA-SHA256 signature; no private key export is
/// possible through this surface.
#[uniffi::export(callback_interface)]
pub trait DeviceSigner: Send + Sync {
    fn public_key_spki_der(&self) -> Result<Vec<u8>, DeviceSignerError>;

    fn sign_p256_sha256(&self, transcript: Vec<u8>) -> Result<Vec<u8>, DeviceSignerError>;
}

#[cfg(test)]
mod tests {
    use super::{DeviceSigner, DeviceSignerError};

    struct RecordingSigner;

    impl DeviceSigner for RecordingSigner {
        fn public_key_spki_der(&self) -> Result<Vec<u8>, DeviceSignerError> {
            Ok(vec![0x30, 0x01])
        }

        fn sign_p256_sha256(&self, transcript: Vec<u8>) -> Result<Vec<u8>, DeviceSignerError> {
            if transcript.is_empty() {
                Err(DeviceSignerError::SigningFailed)
            } else {
                Ok(vec![0x30, 0x02])
            }
        }
    }

    #[test]
    fn the_signer_surface_exports_no_private_key_operation() {
        let signer = RecordingSigner;
        assert_eq!(signer.public_key_spki_der(), Ok(vec![0x30, 0x01]));
        assert_eq!(
            signer.sign_p256_sha256(b"bound transcript".to_vec()),
            Ok(vec![0x30, 0x02])
        );
        assert_eq!(
            signer.sign_p256_sha256(Vec::new()),
            Err(DeviceSignerError::SigningFailed)
        );
    }
}
