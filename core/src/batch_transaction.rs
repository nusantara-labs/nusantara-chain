use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::{
    Hash, Keypair, MerkleProof, MerkleTree, PublicKey, Signature, hash as crypto_hash,
};

use crate::error::CoreError;
use crate::message::Message;
use crate::transaction::Transaction;

/// A single entry in a batch — a message with its Merkle proof.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct BatchEntry {
    pub message: Message,
    pub merkle_proof: MerkleProof,
}

/// A batch of transactions signed with a single Dilithium3 signature.
/// The signer signs the Merkle root of all message hashes.
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct SignedTransactionBatch {
    pub signer_pubkey: PublicKey,
    pub signature: Signature,
    pub merkle_root: Hash,
    pub entries: Vec<BatchEntry>,
}

impl PartialEq for SignedTransactionBatch {
    fn eq(&self, other: &Self) -> bool {
        self.signer_pubkey == other.signer_pubkey
            && self.signature == other.signature
            && self.merkle_root == other.merkle_root
            && self.entries == other.entries
    }
}

impl Eq for SignedTransactionBatch {}

impl SignedTransactionBatch {
    /// Create a new batch from messages, signing the Merkle root.
    pub fn new(messages: Vec<Message>, keypair: &Keypair) -> Result<Self, CoreError> {
        if messages.is_empty() {
            return Err(CoreError::InvalidTransaction(
                "empty batch".to_string(),
            ));
        }

        // Hash each message
        let message_hashes: Vec<Hash> = messages
            .iter()
            .map(|msg| {
                let bytes = borsh::to_vec(msg).expect("message serialization cannot fail");
                crypto_hash(&bytes)
            })
            .collect();

        // Build Merkle tree and sign root
        let tree = MerkleTree::new(&message_hashes);
        let merkle_root = tree.root();
        let signature = keypair.sign(merkle_root.as_bytes());

        // Attach proofs
        let entries: Vec<BatchEntry> = messages
            .into_iter()
            .enumerate()
            .map(|(i, message)| {
                let proof = tree.proof(i).expect("proof index in range");
                BatchEntry {
                    message,
                    merkle_proof: proof,
                }
            })
            .collect();

        Ok(Self {
            signer_pubkey: keypair.public_key().clone(),
            signature,
            merkle_root,
            entries,
        })
    }

    /// Verify the batch signature (1 Dilithium3 verify).
    pub fn verify_signature(&self) -> bool {
        self.signature
            .verify(&self.signer_pubkey, self.merkle_root.as_bytes())
            .is_ok()
    }

    /// Verify a single entry's Merkle proof.
    pub fn verify_entry(&self, index: usize) -> bool {
        if index >= self.entries.len() {
            return false;
        }
        let entry = &self.entries[index];
        let bytes =
            borsh::to_vec(&entry.message).expect("message serialization cannot fail");
        let msg_hash = crypto_hash(&bytes);
        entry.merkle_proof.verify(&msg_hash, &self.merkle_root)
    }

    /// Verify signature + all entry proofs.
    pub fn verify_all(&self) -> bool {
        self.verify_signature()
            && (0..self.entries.len()).all(|i| self.verify_entry(i))
    }

    /// Get the signer's address (hash of pubkey).
    pub fn signer_address(&self) -> Hash {
        crypto_hash(self.signer_pubkey.as_bytes())
    }

    /// Convert batch entries to standalone transactions (for execution).
    /// Each transaction gets the batch signer as the sole signer.
    pub fn to_transactions(&self) -> Vec<Transaction> {
        self.entries
            .iter()
            .map(|entry| Transaction::new(entry.message.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    fn test_message(seed: u8) -> Message {
        Message {
            num_required_signatures: 1,
            num_readonly_signed: 0,
            num_readonly_unsigned: 1,
            account_keys: vec![hash(&[seed]), hash(b"program")],
            recent_blockhash: hash(b"blockhash"),
            instructions: vec![],
        }
    }

    #[test]
    fn create_and_verify() {
        let kp = Keypair::generate();
        let messages = vec![test_message(1), test_message(2), test_message(3)];
        let batch = SignedTransactionBatch::new(messages, &kp).unwrap();

        assert!(batch.verify_signature());
        assert!(batch.verify_all());
        assert_eq!(batch.entries.len(), 3);
    }

    #[test]
    fn verify_individual_entries() {
        let kp = Keypair::generate();
        let messages = vec![test_message(1), test_message(2)];
        let batch = SignedTransactionBatch::new(messages, &kp).unwrap();

        assert!(batch.verify_entry(0));
        assert!(batch.verify_entry(1));
        assert!(!batch.verify_entry(2)); // out of bounds
    }

    #[test]
    fn wrong_signer_fails() {
        let kp = Keypair::generate();
        let kp2 = Keypair::generate();
        let messages = vec![test_message(1)];
        let mut batch = SignedTransactionBatch::new(messages, &kp).unwrap();

        // Tamper: replace pubkey
        batch.signer_pubkey = kp2.public_key().clone();
        assert!(!batch.verify_signature());
    }

    #[test]
    fn tampered_proof_fails() {
        let kp = Keypair::generate();
        let messages = vec![test_message(1), test_message(2)];
        let mut batch = SignedTransactionBatch::new(messages, &kp).unwrap();

        // Tamper: swap proofs
        let proof0 = batch.entries[0].merkle_proof.clone();
        batch.entries[0].merkle_proof = batch.entries[1].merkle_proof.clone();
        batch.entries[1].merkle_proof = proof0;

        // Signature still valid (it's over the root)
        assert!(batch.verify_signature());
        // But individual proofs fail
        assert!(!batch.verify_entry(0));
        assert!(!batch.verify_entry(1));
    }

    #[test]
    fn empty_batch_rejected() {
        let kp = Keypair::generate();
        assert!(SignedTransactionBatch::new(vec![], &kp).is_err());
    }

    #[test]
    fn borsh_roundtrip() {
        let kp = Keypair::generate();
        let messages = vec![test_message(1), test_message(2)];
        let batch = SignedTransactionBatch::new(messages, &kp).unwrap();

        let encoded = borsh::to_vec(&batch).unwrap();
        let decoded: SignedTransactionBatch = borsh::from_slice(&encoded).unwrap();
        assert_eq!(batch, decoded);
        assert!(decoded.verify_all());
    }

    #[test]
    fn signer_address() {
        let kp = Keypair::generate();
        let messages = vec![test_message(1)];
        let batch = SignedTransactionBatch::new(messages, &kp).unwrap();
        assert_eq!(batch.signer_address(), kp.address());
    }

    #[test]
    fn to_transactions_count() {
        let kp = Keypair::generate();
        let messages = vec![test_message(1), test_message(2), test_message(3)];
        let batch = SignedTransactionBatch::new(messages, &kp).unwrap();
        let txs = batch.to_transactions();
        assert_eq!(txs.len(), 3);
    }
}
