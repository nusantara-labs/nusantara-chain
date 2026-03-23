use nusantara_crypto::Hash;

const HASH_BYTES: usize = 64;

/// Encode a slot as 8-byte big-endian key.
pub fn slot_key(slot: u64) -> [u8; 8] {
    slot.to_be_bytes()
}

/// Encode an account key: address(64) ++ slot(8 BE).
pub fn account_key(address: &Hash, slot: u64) -> [u8; 72] {
    let mut key = [0u8; HASH_BYTES + 8];
    key[..HASH_BYTES].copy_from_slice(address.as_bytes());
    key[HASH_BYTES..].copy_from_slice(&slot.to_be_bytes());
    key
}

/// Encode an address-signature key: address(64) ++ slot(8 BE) ++ tx_index(4 BE).
pub fn address_sig_key(address: &Hash, slot: u64, tx_index: u32) -> [u8; 76] {
    let mut key = [0u8; HASH_BYTES + 8 + 4];
    key[..HASH_BYTES].copy_from_slice(address.as_bytes());
    key[HASH_BYTES..HASH_BYTES + 8].copy_from_slice(&slot.to_be_bytes());
    key[HASH_BYTES + 8..].copy_from_slice(&tx_index.to_be_bytes());
    key
}

/// Encode a shred key: slot(8 BE) ++ shred_index(4 BE).
pub fn shred_key(slot: u64, index: u32) -> [u8; 12] {
    let mut key = [0u8; 12];
    key[..8].copy_from_slice(&slot.to_be_bytes());
    key[8..].copy_from_slice(&index.to_be_bytes());
    key
}

/// Encode an owner/program index key: prefix_hash(64) ++ account_address(64).
pub fn owner_index_key(prefix: &Hash, address: &Hash) -> [u8; 128] {
    let mut key = [0u8; HASH_BYTES + HASH_BYTES];
    key[..HASH_BYTES].copy_from_slice(prefix.as_bytes());
    key[HASH_BYTES..].copy_from_slice(address.as_bytes());
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    #[test]
    fn slot_key_ordering() {
        let k1 = slot_key(0);
        let k2 = slot_key(1);
        let k3 = slot_key(u64::MAX);
        assert!(k1 < k2);
        assert!(k2 < k3);
    }

    #[test]
    fn account_key_layout() {
        let addr = hash(b"test");
        let key = account_key(&addr, 42);
        assert_eq!(key.len(), 72);
        assert_eq!(&key[..64], addr.as_bytes());
        assert_eq!(&key[64..], &42u64.to_be_bytes());
    }

    #[test]
    fn address_sig_key_layout() {
        let addr = hash(b"addr");
        let key = address_sig_key(&addr, 100, 5);
        assert_eq!(key.len(), 76);
        assert_eq!(&key[..64], addr.as_bytes());
        assert_eq!(&key[64..72], &100u64.to_be_bytes());
        assert_eq!(&key[72..76], &5u32.to_be_bytes());
    }

    #[test]
    fn shred_key_layout() {
        let key = shred_key(10, 3);
        assert_eq!(key.len(), 12);
        assert_eq!(&key[..8], &10u64.to_be_bytes());
        assert_eq!(&key[8..], &3u32.to_be_bytes());
    }

    #[test]
    fn account_key_lexicographic_ordering_by_slot() {
        let addr = hash(b"same");
        let k1 = account_key(&addr, 1);
        let k2 = account_key(&addr, 2);
        let k3 = account_key(&addr, 100);
        assert!(k1 < k2);
        assert!(k2 < k3);
    }

    #[test]
    fn owner_index_key_layout() {
        let owner = hash(b"owner");
        let addr = hash(b"account");
        let key = owner_index_key(&owner, &addr);
        assert_eq!(key.len(), 128);
        assert_eq!(&key[..64], owner.as_bytes());
        assert_eq!(&key[64..], addr.as_bytes());
    }
}
