use pqcrypto_dilithium::dilithium3;
use pqcrypto_traits::sign::SecretKey as PqSecretKey;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::account_id::AccountId;
use crate::error::CryptoError;
use crate::hash::Hash;
use crate::pubkey::PublicKey;
use crate::signature::Signature;

pub const SECRET_KEY_BYTES: usize = 4032;

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SecretKey(Vec<u8>);

impl SecretKey {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CryptoError> {
        if bytes.len() != SECRET_KEY_BYTES {
            return Err(CryptoError::InvalidSecretKeyLength {
                expected: SECRET_KEY_BYTES,
                got: bytes.len(),
            });
        }
        Ok(Self(bytes.to_vec()))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    fn to_pq(&self) -> Result<dilithium3::SecretKey, CryptoError> {
        dilithium3::SecretKey::from_bytes(&self.0).map_err(|_| CryptoError::InvalidKeyBytes)
    }
}

pub struct Keypair {
    public: PublicKey,
    secret: SecretKey,
}

impl Keypair {
    pub fn generate() -> Self {
        let (pq_pk, pq_sk) = dilithium3::keypair();
        let public = PublicKey::from_pq(&pq_pk);
        let secret = SecretKey(pqcrypto_traits::sign::SecretKey::as_bytes(&pq_sk).to_vec());
        Self { public, secret }
    }

    pub fn from_bytes(pubkey_bytes: &[u8], secret_bytes: &[u8]) -> Result<Self, CryptoError> {
        let public = PublicKey::from_bytes(pubkey_bytes)?;
        let secret = SecretKey::from_bytes(secret_bytes)?;
        Ok(Self { public, secret })
    }

    pub fn public_key(&self) -> &PublicKey {
        &self.public
    }

    pub fn secret_key(&self) -> &SecretKey {
        &self.secret
    }

    pub fn address(&self) -> Hash {
        self.public.to_address()
    }

    pub fn account_id(&self) -> AccountId {
        self.public.to_account_id()
    }

    pub fn sign(&self, message: &[u8]) -> Signature {
        let pq_sk = self.secret.to_pq().expect("secret key was validated on creation");
        let sig = dilithium3::detached_sign(message, &pq_sk);
        Signature::from_pq(&sig)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_and_sign_verify() {
        let kp = Keypair::generate();
        let msg = b"hello nusantara";
        let sig = kp.sign(msg);
        assert!(sig.verify(kp.public_key(), msg).is_ok());
    }

    #[test]
    fn wrong_message_fails_verification() {
        let kp = Keypair::generate();
        let sig = kp.sign(b"correct");
        assert!(sig.verify(kp.public_key(), b"wrong").is_err());
    }

    #[test]
    fn from_bytes_roundtrip() {
        let kp = Keypair::generate();
        let pub_bytes = kp.public_key().as_bytes().to_vec();
        let sec_bytes = kp.secret_key().as_bytes().to_vec();
        let kp2 = Keypair::from_bytes(&pub_bytes, &sec_bytes).unwrap();
        assert_eq!(kp.public_key(), kp2.public_key());

        let msg = b"test";
        let sig = kp2.sign(msg);
        assert!(sig.verify(kp.public_key(), msg).is_ok());
    }

    #[test]
    fn public_key_length() {
        let kp = Keypair::generate();
        assert_eq!(
            kp.public_key().as_bytes().len(),
            crate::pubkey::PUBLIC_KEY_BYTES
        );
    }

    #[test]
    fn secret_key_length() {
        let kp = Keypair::generate();
        assert_eq!(kp.secret_key().as_bytes().len(), SECRET_KEY_BYTES);
    }
}
