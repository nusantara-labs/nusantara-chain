use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::Hash;

use crate::cf::CF_SLASHES;
use crate::error::StorageError;
use crate::storage::Storage;

/// Proof of equivocation (double-voting) by a validator at a given slot.
///
/// Records the two conflicting block hashes the validator voted for,
/// the reporter who detected the violation, and the detection timestamp.
/// Signatures are omitted to keep the proof compact (Dilithium3 sigs are 3309 bytes each).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SlashProof {
    /// Identity hash of the validator that double-voted.
    pub validator: Hash,
    /// Slot in which the equivocation occurred.
    pub slot: u64,
    /// Block hash from the first vote observed.
    pub vote1_hash: Hash,
    /// Block hash from the conflicting second vote.
    pub vote2_hash: Hash,
    /// Identity hash of the node that detected the equivocation.
    pub reporter: Hash,
    /// Unix timestamp (seconds) when the equivocation was detected.
    pub timestamp: i64,
}

/// Build the RocksDB key for a slash proof: validator_hash(64) ++ slot(8 BE).
fn slash_proof_key(validator: &Hash, slot: u64) -> [u8; 72] {
    let mut key = [0u8; 72];
    key[..64].copy_from_slice(validator.as_bytes());
    key[64..].copy_from_slice(&slot.to_be_bytes());
    key
}

impl Storage {
    /// Persist a slash proof for a validator.
    pub fn put_slash_proof(&self, proof: &SlashProof) -> Result<(), StorageError> {
        let key = slash_proof_key(&proof.validator, proof.slot);
        let value = borsh::to_vec(proof).map_err(|e| StorageError::Serialization(e.to_string()))?;
        self.put_cf(CF_SLASHES, &key, &value)
    }

    /// Retrieve all slash proofs for a given validator, ordered by slot.
    pub fn get_slash_proofs(&self, validator: &Hash) -> Result<Vec<SlashProof>, StorageError> {
        let cf = self
            .db
            .cf_handle(CF_SLASHES)
            .ok_or(StorageError::CfNotFound(CF_SLASHES))?;
        let prefix = validator.as_bytes();
        let iter = self.db.prefix_iterator_cf(cf, prefix);
        let mut results = Vec::new();
        for item in iter {
            let (key, value) = item.map_err(StorageError::RocksDb)?;
            // Stop when we leave the prefix
            if key.len() < 64 || key[..64] != *prefix {
                break;
            }
            let proof = SlashProof::try_from_slice(&value)
                .map_err(|e| StorageError::Deserialization(e.to_string()))?;
            results.push(proof);
        }
        Ok(results)
    }

    /// Get a single slash proof for a specific validator and slot.
    pub fn get_slash_proof(
        &self,
        validator: &Hash,
        slot: u64,
    ) -> Result<Option<SlashProof>, StorageError> {
        let key = slash_proof_key(validator, slot);
        match self.get_cf(CF_SLASHES, &key)? {
            Some(bytes) => {
                let proof = SlashProof::try_from_slice(&bytes)
                    .map_err(|e| StorageError::Deserialization(e.to_string()))?;
                Ok(Some(proof))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    fn temp_storage() -> (Storage, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        (storage, dir)
    }

    #[test]
    fn put_and_get_slash_proof() {
        let (storage, _dir) = temp_storage();
        let validator = hash(b"bad_validator");
        let proof = SlashProof {
            validator,
            slot: 42,
            vote1_hash: hash(b"block_a"),
            vote2_hash: hash(b"block_b"),
            reporter: hash(b"reporter"),
            timestamp: 1_700_000_000,
        };

        storage.put_slash_proof(&proof).unwrap();

        let retrieved = storage.get_slash_proof(&validator, 42).unwrap();
        assert_eq!(retrieved, Some(proof.clone()));

        let all = storage.get_slash_proofs(&validator).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0], proof);
    }

    #[test]
    fn multiple_proofs_same_validator() {
        let (storage, _dir) = temp_storage();
        let validator = hash(b"repeat_offender");

        for slot in [10, 20, 30] {
            let proof = SlashProof {
                validator,
                slot,
                vote1_hash: hash(format!("block_a_{slot}").as_bytes()),
                vote2_hash: hash(format!("block_b_{slot}").as_bytes()),
                reporter: hash(b"reporter"),
                timestamp: 1_700_000_000 + slot as i64,
            };
            storage.put_slash_proof(&proof).unwrap();
        }

        let proofs = storage.get_slash_proofs(&validator).unwrap();
        assert_eq!(proofs.len(), 3);
        // Ordered by slot (BE encoding preserves order)
        assert_eq!(proofs[0].slot, 10);
        assert_eq!(proofs[1].slot, 20);
        assert_eq!(proofs[2].slot, 30);
    }

    #[test]
    fn get_proof_missing_returns_none() {
        let (storage, _dir) = temp_storage();
        let validator = hash(b"honest_validator");
        let result = storage.get_slash_proof(&validator, 99).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn proofs_isolated_per_validator() {
        let (storage, _dir) = temp_storage();
        let val_a = hash(b"validator_a");
        let val_b = hash(b"validator_b");

        storage
            .put_slash_proof(&SlashProof {
                validator: val_a,
                slot: 1,
                vote1_hash: hash(b"a1"),
                vote2_hash: hash(b"a2"),
                reporter: hash(b"r"),
                timestamp: 100,
            })
            .unwrap();

        storage
            .put_slash_proof(&SlashProof {
                validator: val_b,
                slot: 2,
                vote1_hash: hash(b"b1"),
                vote2_hash: hash(b"b2"),
                reporter: hash(b"r"),
                timestamp: 200,
            })
            .unwrap();

        let proofs_a = storage.get_slash_proofs(&val_a).unwrap();
        assert_eq!(proofs_a.len(), 1);
        assert_eq!(proofs_a[0].validator, val_a);

        let proofs_b = storage.get_slash_proofs(&val_b).unwrap();
        assert_eq!(proofs_b.len(), 1);
        assert_eq!(proofs_b[0].validator, val_b);
    }

    #[test]
    fn borsh_roundtrip() {
        let proof = SlashProof {
            validator: hash(b"val"),
            slot: 999,
            vote1_hash: hash(b"h1"),
            vote2_hash: hash(b"h2"),
            reporter: hash(b"rep"),
            timestamp: 1_234_567_890,
        };
        let bytes = borsh::to_vec(&proof).unwrap();
        let decoded = SlashProof::try_from_slice(&bytes).unwrap();
        assert_eq!(proof, decoded);
    }
}
