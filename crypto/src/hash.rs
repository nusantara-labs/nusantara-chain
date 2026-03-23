use std::fmt;
use std::io::{self, Write};
use std::str::FromStr;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use borsh::{BorshDeserialize, BorshSerialize};
use sha3::{Digest, Sha3_512};

use crate::error::CryptoError;

pub const HASH_BYTES: usize = 64;

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hash(pub(crate) [u8; HASH_BYTES]);

impl BorshSerialize for Hash {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&self.0)
    }
}

impl BorshDeserialize for Hash {
    fn deserialize_reader<R: io::Read>(reader: &mut R) -> io::Result<Self> {
        let mut buf = [0u8; HASH_BYTES];
        reader.read_exact(&mut buf)?;
        Ok(Self(buf))
    }
}

impl Hash {
    pub fn new(bytes: [u8; HASH_BYTES]) -> Self {
        Self(bytes)
    }

    pub fn zero() -> Self {
        Self([0u8; HASH_BYTES])
    }

    pub fn as_bytes(&self) -> &[u8; HASH_BYTES] {
        &self.0
    }

    pub fn to_base64(&self) -> String {
        base64_encode(&self.0)
    }

    pub fn from_base64(s: &str) -> Result<Self, CryptoError> {
        let bytes = base64_decode(s)?;
        let arr: [u8; HASH_BYTES] =
            bytes
                .try_into()
                .map_err(|v: Vec<u8>| CryptoError::InvalidHashLength {
                    expected: HASH_BYTES,
                    got: v.len(),
                })?;
        Ok(Self(arr))
    }


}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b64 = self.to_base64();
        write!(f, "Hash({}...)", &b64[..8])
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_base64())
    }
}

impl FromStr for Hash {
    type Err = CryptoError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_base64(s)
    }
}

impl AsRef<[u8]> for Hash {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

pub fn hash(data: &[u8]) -> Hash {
    let result = Sha3_512::digest(data);
    let mut bytes = [0u8; HASH_BYTES];
    bytes.copy_from_slice(&result);
    Hash(bytes)
}

pub fn hashv(slices: &[&[u8]]) -> Hash {
    let mut hasher = Sha3_512::new();
    for slice in slices {
        hasher.update(slice);
    }
    let result = hasher.finalize();
    let mut bytes = [0u8; HASH_BYTES];
    bytes.copy_from_slice(&result);
    Hash(bytes)
}

pub struct Hasher {
    inner: Sha3_512,
}

impl Hasher {
    pub fn new() -> Self {
        Self {
            inner: Sha3_512::new(),
        }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    pub fn finalize(self) -> Hash {
        let result = self.inner.finalize();
        let mut bytes = [0u8; HASH_BYTES];
        bytes.copy_from_slice(&result);
        Hash(bytes)
    }
}

impl Default for Hasher {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn base64_encode(data: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(data)
}

pub(crate) fn base64_decode(s: &str) -> Result<Vec<u8>, CryptoError> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|e| CryptoError::InvalidBase64(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_deterministic() {
        let h1 = hash(b"hello");
        let h2 = hash(b"hello");
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_different_inputs() {
        let h1 = hash(b"hello");
        let h2 = hash(b"world");
        assert_ne!(h1, h2);
    }

    #[test]
    fn hashv_matches_concatenated() {
        let h1 = hashv(&[b"hello", b"world"]);
        let h2 = hash(b"helloworld");
        assert_eq!(h1, h2);
    }

    #[test]
    fn hasher_incremental() {
        let mut hasher = Hasher::new();
        hasher.update(b"hello");
        hasher.update(b"world");
        let h1 = hasher.finalize();
        let h2 = hash(b"helloworld");
        assert_eq!(h1, h2);
    }

    #[test]
    fn base64_roundtrip() {
        let h = hash(b"test");
        let encoded = h.to_base64();
        let decoded = Hash::from_base64(&encoded).unwrap();
        assert_eq!(h, decoded);
    }

    #[test]
    fn display_fromstr_roundtrip() {
        let h = hash(b"roundtrip");
        let s = h.to_string();
        let parsed: Hash = s.parse().unwrap();
        assert_eq!(h, parsed);
    }

    #[test]
    fn debug_format() {
        let h = hash(b"debug");
        let debug = format!("{h:?}");
        assert!(debug.starts_with("Hash("));
        assert!(debug.ends_with("...)"));
    }

    #[test]
    fn zero_hash() {
        let z = Hash::zero();
        assert_eq!(z.as_bytes(), &[0u8; HASH_BYTES]);
        assert_ne!(z, hash(b""));
    }

    #[test]
    fn hash_output_is_64_bytes() {
        let h = hash(b"any data");
        assert_eq!(h.as_bytes().len(), 64);
    }

    #[test]
    fn base64_url_no_padding() {
        let h = hash(b"check padding");
        let encoded = h.to_base64();
        assert!(!encoded.contains('='));
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
    }

    #[test]
    fn borsh_roundtrip() {
        let h = hash(b"borsh test");
        let encoded = borsh::to_vec(&h).unwrap();
        assert_eq!(encoded.len(), HASH_BYTES);
        let decoded: Hash = borsh::from_slice(&encoded).unwrap();
        assert_eq!(h, decoded);
    }
}
