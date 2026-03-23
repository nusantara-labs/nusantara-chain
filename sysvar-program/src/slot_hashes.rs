use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::{Hash, hash};

use crate::Sysvar;

pub type SlotHash = (u64, Hash);

#[derive(Clone, Debug, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SlotHashes(pub Vec<SlotHash>);

impl SlotHashes {
    pub fn new(slot_hashes: Vec<SlotHash>) -> Self {
        Self(slot_hashes)
    }

    pub fn get(&self, slot: u64) -> Option<&Hash> {
        self.0
            .iter()
            .find(|(s, _)| *s == slot)
            .map(|(_, h)| h)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Sysvar for SlotHashes {
    fn id() -> Hash {
        hash(b"sysvar_slot_hashes")
    }

    fn size_of() -> usize {
        // Variable size, but provide max estimate
        // Each entry: u64 (8) + Hash (64) = 72 bytes
        // Max 512 entries: 512 * 72 + 4 (vec len) = 36868
        36868
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash as crypto_hash;

    #[test]
    fn borsh_roundtrip() {
        let hashes = SlotHashes::new(vec![
            (100, crypto_hash(b"slot100")),
            (99, crypto_hash(b"slot99")),
        ]);
        let encoded = borsh::to_vec(&hashes).unwrap();
        let decoded: SlotHashes = borsh::from_slice(&encoded).unwrap();
        assert_eq!(hashes, decoded);
    }

    #[test]
    fn get_slot() {
        let h = crypto_hash(b"slot42");
        let hashes = SlotHashes::new(vec![(42, h)]);
        assert_eq!(hashes.get(42), Some(&h));
        assert_eq!(hashes.get(99), None);
    }
}
