use ed25519_dalek::{Signature, Signer, SigningKey};
use rand::rngs::OsRng;

// Re-export VerifyingKey so it can be used by other modules
pub use ed25519_dalek::VerifyingKey;

#[derive(Clone)]
pub struct KeyPair {
    pub signing_key: SigningKey,
    pub verifying_key: VerifyingKey,
}

impl KeyPair {
    pub fn generate() -> Self {
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let verifying_key = signing_key.verifying_key();
        Self {
            signing_key,
            verifying_key,
        }
    }

    pub fn from_seed(seed: &[u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(seed);
        let verifying_key = signing_key.verifying_key();
        Self {
            signing_key,
            verifying_key,
        }
    }

    pub fn sign(&self, message: &[u8]) -> Signature {
        self.signing_key.sign(message)
    }

    pub fn verify(verifying_key: &VerifyingKey, message: &[u8], signature: &Signature) -> bool {
        verifying_key.verify_strict(message, signature).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keypair_generation() {
        let keypair = KeyPair::generate();
        // Verifying key should be derivable from signing key
        assert_eq!(keypair.verifying_key, keypair.signing_key.verifying_key());
    }

    #[test]
    fn test_sign_and_verify() {
        let keypair = KeyPair::generate();
        let message = b"test message";

        let signature = keypair.sign(message);

        // Should verify correctly
        assert!(KeyPair::verify(&keypair.verifying_key, message, &signature));

        // Should fail with wrong message
        assert!(!KeyPair::verify(
            &keypair.verifying_key,
            b"wrong message",
            &signature
        ));

        // Should fail with wrong key
        let other_keypair = KeyPair::generate();
        assert!(!KeyPair::verify(
            &other_keypair.verifying_key,
            message,
            &signature
        ));
    }

    #[test]
    fn test_multiple_keypairs_unique() {
        let keypair1 = KeyPair::generate();
        let keypair2 = KeyPair::generate();

        // Keys should be different
        assert_ne!(keypair1.verifying_key, keypair2.verifying_key);
    }
}
