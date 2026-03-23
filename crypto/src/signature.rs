use std::fmt;
use std::io::{self, Write};
use std::str::FromStr;

use borsh::{BorshDeserialize, BorshSerialize};
use pqcrypto_dilithium::dilithium3;
use pqcrypto_traits::sign::DetachedSignature;

use crate::error::CryptoError;
use crate::hash::{base64_decode, base64_encode};
use crate::pubkey::PublicKey;

pub const SIGNATURE_BYTES: usize = 3309;

#[derive(Clone, PartialEq, Eq)]
pub struct Signature(Vec<u8>);

impl BorshSerialize for Signature {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        BorshSerialize::serialize(&self.0, writer)
    }
}

impl BorshDeserialize for Signature {
    fn deserialize_reader<R: io::Read>(reader: &mut R) -> io::Result<Self> {
        let bytes: Vec<u8> = BorshDeserialize::deserialize_reader(reader)?;
        if bytes.len() != SIGNATURE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "invalid signature length: expected {SIGNATURE_BYTES}, got {}",
                    bytes.len()
                ),
            ));
        }
        Ok(Self(bytes))
    }
}

impl Signature {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CryptoError> {
        if bytes.len() != SIGNATURE_BYTES {
            return Err(CryptoError::InvalidSignatureLength {
                expected: SIGNATURE_BYTES,
                got: bytes.len(),
            });
        }
        Ok(Self(bytes.to_vec()))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn to_base64(&self) -> String {
        base64_encode(&self.0)
    }

    pub fn from_base64(s: &str) -> Result<Self, CryptoError> {
        let bytes = base64_decode(s)?;
        Self::from_bytes(&bytes)
    }

    pub fn verify(&self, pubkey: &PublicKey, message: &[u8]) -> Result<(), CryptoError> {
        let pq_pk = pubkey.to_pq()?;
        let pq_sig = dilithium3::DetachedSignature::from_bytes(&self.0)
            .map_err(|_| CryptoError::VerificationFailed)?;
        dilithium3::verify_detached_signature(&pq_sig, message, &pq_pk)
            .map_err(|_| CryptoError::VerificationFailed)
    }

    pub(crate) fn from_pq(sig: &dilithium3::DetachedSignature) -> Self {
        Self(pqcrypto_traits::sign::DetachedSignature::as_bytes(sig).to_vec())
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b64 = self.to_base64();
        write!(f, "Signature({}...)", &b64[..8])
    }
}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_base64())
    }
}

impl FromStr for Signature {
    type Err = CryptoError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_base64(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_bytes_wrong_length() {
        let err = Signature::from_bytes(&[0u8; 100]).unwrap_err();
        assert!(matches!(err, CryptoError::InvalidSignatureLength { .. }));
    }
}
