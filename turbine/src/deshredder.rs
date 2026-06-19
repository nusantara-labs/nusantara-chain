use nusantara_core::block::Block;
use nusantara_storage::shred::DataShred;

use crate::error::TurbineError;
use crate::merkle_shred::LAST_SHRED_FLAG;

pub struct Deshredder;

impl Deshredder {
    /// Reassemble a block from data shreds.
    /// Shreds must all share the same slot, be sorted by index, and be contiguous from 0.
    pub fn deshred(shreds: &[DataShred]) -> Result<Block, TurbineError> {
        if shreds.is_empty() {
            return Err(TurbineError::Deshredding(
                "no shreds provided".to_string(),
            ));
        }

        // Verify all shreds share the same slot (item 23)
        let slot = shreds[0].slot;
        for shred in shreds {
            if shred.slot != slot {
                return Err(TurbineError::Deshredding(format!(
                    "mixed slots in shred batch: expected slot {slot}, got {}",
                    shred.slot
                )));
            }
        }

        // Verify contiguous indices
        for (i, shred) in shreds.iter().enumerate() {
            if shred.index != i as u32 {
                return Err(TurbineError::Deshredding(format!(
                    "expected shred index {i}, got {}",
                    shred.index
                )));
            }
        }

        // Verify last shred has the last flag
        let last = shreds.last().expect("non-empty checked above");
        if last.flags & LAST_SHRED_FLAG == 0 {
            return Err(TurbineError::Deshredding(
                "last shred missing completion flag".to_string(),
            ));
        }

        // Concatenate data
        let total_size: usize = shreds.iter().map(|s| s.data.len()).sum();
        let mut block_bytes = Vec::with_capacity(total_size);
        for shred in shreds {
            block_bytes.extend_from_slice(&shred.data);
        }

        // Deserialize block
        let block: Block = borsh::from_slice(&block_bytes)
            .map_err(|e| TurbineError::Deshredding(e.to_string()))?;

        Ok(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shredder::Shredder;
    use nusantara_core::block::{Block, BlockHeader};
    use nusantara_crypto::{Hash, Keypair, hash};

    fn test_block(slot: u64) -> Block {
        Block {
            header: BlockHeader {
                slot,
                parent_slot: slot.saturating_sub(1),
                parent_hash: hash(b"parent"),
                block_hash: hash(b"block"),
                timestamp: 1000,
                validator: hash(b"validator"),
                transaction_count: 0,
                merkle_root: Hash::zero(),
                poh_hash: Hash::zero(),
                bank_hash: Hash::zero(),
                state_root: Hash::zero(),
            },
            transactions: Vec::new(),
            batches: Vec::new(),
        }
    }

    #[test]
    fn shred_deshred_roundtrip() {
        let kp = Keypair::generate();
        let original = test_block(1);

        let batch = Shredder::shred_block(&original, 0, &kp).unwrap();
        let data_shreds: Vec<DataShred> = batch
            .data_shreds
            .iter()
            .map(|s| s.shred.clone())
            .collect();

        let recovered = Deshredder::deshred(&data_shreds).unwrap();
        assert_eq!(original, recovered);
    }

    #[test]
    fn empty_shreds_error() {
        let result = Deshredder::deshred(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn non_contiguous_indices_error() {
        let shreds = vec![
            DataShred {
                slot: 1,
                index: 0,
                parent_offset: 1,
                data: vec![0],
                flags: 0,
            },
            DataShred {
                slot: 1,
                index: 2, // gap!
                parent_offset: 1,
                data: vec![0],
                flags: LAST_SHRED_FLAG,
            },
        ];
        let result = Deshredder::deshred(&shreds);
        assert!(result.is_err());
    }

    #[test]
    fn mixed_slots_error() {
        let shreds = vec![
            DataShred {
                slot: 1,
                index: 0,
                parent_offset: 1,
                data: vec![0],
                flags: 0,
            },
            DataShred {
                slot: 2, // wrong slot
                index: 1,
                parent_offset: 1,
                data: vec![0],
                flags: LAST_SHRED_FLAG,
            },
        ];
        let result = Deshredder::deshred(&shreds);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("mixed slots"), "got: {msg}");
    }
}
