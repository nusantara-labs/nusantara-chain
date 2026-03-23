use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::{Hash, PublicKey, hash as crypto_hash};

use crate::batch_transaction::SignedTransactionBatch;
use crate::transaction::Transaction;

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct BlockHeader {
    pub slot: u64,
    pub parent_slot: u64,
    pub parent_hash: Hash,
    pub block_hash: Hash,
    pub timestamp: i64,
    pub validator: Hash,
    pub transaction_count: u64,
    pub merkle_root: Hash,
    /// Final PoH hash for this slot.
    pub poh_hash: Hash,
    /// Bank hash = hashv(parent_bank_hash, account_delta_hash).
    pub bank_hash: Hash,
    /// Merkle root of the full account state tree after this slot.
    pub state_root: Hash,
}

impl BlockHeader {
    pub fn verify_validator(&self, pubkey: &PublicKey) -> bool {
        crypto_hash(pubkey.as_bytes()) == self.validator
    }
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
    pub batches: Vec<SignedTransactionBatch>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::{Keypair, hash};

    #[test]
    fn verify_validator_address() {
        let kp = Keypair::generate();
        let header = BlockHeader {
            slot: 0,
            parent_slot: 0,
            parent_hash: Hash::zero(),
            block_hash: hash(b"block"),
            timestamp: 1000,
            validator: kp.address(),
            transaction_count: 0,
            merkle_root: Hash::zero(),
            poh_hash: Hash::zero(),
            bank_hash: Hash::zero(),
            state_root: Hash::zero(),
        };
        assert!(header.verify_validator(kp.public_key()));

        let other_kp = Keypair::generate();
        assert!(!header.verify_validator(other_kp.public_key()));
    }

    #[test]
    fn borsh_roundtrip() {
        let header = BlockHeader {
            slot: 42,
            parent_slot: 41,
            parent_hash: hash(b"parent"),
            block_hash: hash(b"block"),
            timestamp: 1234567890,
            validator: hash(b"validator"),
            transaction_count: 10,
            merkle_root: hash(b"merkle"),
            poh_hash: hash(b"poh"),
            bank_hash: hash(b"bank"),
            state_root: Hash::zero(),
        };
        let block = Block {
            header,
            transactions: Vec::new(),
            batches: Vec::new(),
        };
        let encoded = borsh::to_vec(&block).unwrap();
        let decoded: Block = borsh::from_slice(&encoded).unwrap();
        assert_eq!(block, decoded);
    }
}
