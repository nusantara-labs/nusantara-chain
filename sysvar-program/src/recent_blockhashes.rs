use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::{Hash, hash};

use crate::Sysvar;

#[derive(Clone, Debug, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct RecentBlockhashes(pub Vec<Hash>);

impl RecentBlockhashes {
    pub fn new(hashes: Vec<Hash>) -> Self {
        Self(hashes)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, blockhash: &Hash) -> bool {
        self.0.contains(blockhash)
    }
}

impl Sysvar for RecentBlockhashes {
    fn id() -> Hash {
        hash(b"sysvar_recent_blockhashes")
    }

    fn size_of() -> usize {
        // Max 300 hashes * 64 bytes + 4 (vec len) = 19204
        19204
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash as crypto_hash;

    #[test]
    fn borsh_roundtrip() {
        let hashes = RecentBlockhashes::new(vec![
            crypto_hash(b"block1"),
            crypto_hash(b"block2"),
        ]);
        let encoded = borsh::to_vec(&hashes).unwrap();
        let decoded: RecentBlockhashes = borsh::from_slice(&encoded).unwrap();
        assert_eq!(hashes, decoded);
    }

    #[test]
    fn contains() {
        let h = crypto_hash(b"target");
        let hashes = RecentBlockhashes::new(vec![h]);
        assert!(hashes.contains(&h));
        assert!(!hashes.contains(&crypto_hash(b"other")));
    }
}
