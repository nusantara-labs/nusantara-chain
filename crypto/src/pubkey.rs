use std::fmt;
use std::io::{self, Write};
use std::str::FromStr;

use borsh::{BorshDeserialize, BorshSerialize};
use pqcrypto_dilithium::dilithium3;
use pqcrypto_traits::sign::PublicKey as PqPublicKey;

use crate::account_id::AccountId;
use crate::error::CryptoError;
use crate::hash::{Hash, base64_decode, base64_encode, hash};

pub const PUBLIC_KEY_BYTES: usize = 1952;

#[derive(Clone, PartialEq, Eq)]
pub struct PublicKey(Vec<u8>);

impl BorshSerialize for PublicKey {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        BorshSerialize::serialize(&self.0, writer)
    }
}

impl BorshDeserialize for PublicKey {
    fn deserialize_reader<R: io::Read>(reader: &mut R) -> io::Result<Self> {
        let bytes: Vec<u8> = BorshDeserialize::deserialize_reader(reader)?;
        if bytes.len() != PUBLIC_KEY_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "invalid public key length: expected {PUBLIC_KEY_BYTES}, got {}",
                    bytes.len()
                ),
            ));
        }
        Ok(Self(bytes))
    }
}

impl PublicKey {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CryptoError> {
        if bytes.len() != PUBLIC_KEY_BYTES {
            return Err(CryptoError::InvalidPublicKeyLength {
                expected: PUBLIC_KEY_BYTES,
                got: bytes.len(),
            });
        }
        Ok(Self(bytes.to_vec()))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn to_address(&self) -> Hash {
        hash(&self.0)
    }

    pub fn to_account_id(&self) -> AccountId {
        AccountId::Implicit(self.to_address())
    }

    pub fn to_base64(&self) -> String {
        base64_encode(&self.0)
    }

    pub fn from_base64(s: &str) -> Result<Self, CryptoError> {
        let bytes = base64_decode(s)?;
        Self::from_bytes(&bytes)
    }

    pub(crate) fn from_pq(pk: &dilithium3::PublicKey) -> Self {
        Self(pqcrypto_traits::sign::PublicKey::as_bytes(pk).to_vec())
    }

    pub(crate) fn to_pq(&self) -> Result<dilithium3::PublicKey, CryptoError> {
        dilithium3::PublicKey::from_bytes(&self.0).map_err(|_| CryptoError::InvalidKeyBytes)
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b64 = self.to_base64();
        write!(f, "PublicKey({}...)", &b64[..8])
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_base64())
    }
}

impl FromStr for PublicKey {
    type Err = CryptoError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_base64(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Keypair;

    #[test]
    fn from_bytes_wrong_length() {
        let err = PublicKey::from_bytes(&[0u8; 100]).unwrap_err();
        assert!(matches!(err, CryptoError::InvalidPublicKeyLength { .. }));
    }

    #[test]
    fn base64_roundtrip() {
        let kp = Keypair::generate();
        let pk = kp.public_key().clone();
        let encoded = pk.to_base64();
        let decoded = PublicKey::from_base64(&encoded).unwrap();
        assert_eq!(pk, decoded);
    }

    #[test]
    fn borsh_roundtrip() {
        let kp = Keypair::generate();
        let pk = kp.public_key().clone();
        let encoded = borsh::to_vec(&pk).unwrap();
        assert_eq!(encoded.len(), 4 + PUBLIC_KEY_BYTES);
        let decoded: PublicKey = borsh::from_slice(&encoded).unwrap();
        assert_eq!(pk, decoded);
    }
}
