use nusantara_core::block::Block;
use nusantara_core::native_token::const_parse_u64;
use nusantara_crypto::{Keypair, MerkleTree};
use nusantara_storage::shred::{CodeShred, DataShred};

use crate::erasure::ErasureCoder;
use crate::error::TurbineError;
use crate::merkle_shred::{
    MerkleCodeShred, MerkleDataShred, ShredBatchHeader, build_batch_header,
};

pub const MAX_DATA_PER_SHRED: u64 = const_parse_u64(env!("NUSA_TURBINE_MAX_DATA_PER_SHRED"));
pub const FEC_RATE_PERCENT: u64 = const_parse_u64(env!("NUSA_TURBINE_FEC_RATE_PERCENT"));

pub struct ShredBatch {
    pub header: ShredBatchHeader,
    pub data_shreds: Vec<MerkleDataShred>,
    pub code_shreds: Vec<MerkleCodeShred>,
}

pub struct Shredder;

impl Shredder {
    /// Shred a block into Merkle-authenticated data shreds + FEC code shreds.
    /// One Dilithium3 signature per slot (on the Merkle root), instead of per shred.
    pub fn shred_block(
        block: &Block,
        parent_slot: u64,
        keypair: &Keypair,
    ) -> Result<ShredBatch, TurbineError> {
        let slot = block.header.slot;
        let leader = keypair.address();
        let block_bytes = borsh::to_vec(block)
            .map_err(|e| TurbineError::BlockSerialization(e.to_string()))?;

        let chunk_size = MAX_DATA_PER_SHRED as usize;
        let chunks: Vec<&[u8]> = block_bytes.chunks(chunk_size).collect();
        let num_chunks = chunks.len();

        // Step 1: Create data shreds (unsigned — proofs attached later)
        let mut data_shreds = Vec::with_capacity(num_chunks);
        for (i, chunk) in chunks.iter().enumerate() {
            let is_last = i == num_chunks - 1;
            let shred = DataShred {
                slot,
                index: i as u32,
                parent_offset: slot.checked_sub(parent_slot)
                    .and_then(|d| u16::try_from(d).ok())
                    .unwrap_or(0),
                data: chunk.to_vec(),
                flags: if is_last { 0x01 } else { 0x00 },
            };
            data_shreds.push(MerkleDataShred::new(shred, leader));
        }

        // Step 2: FEC encode in groups of 32 data shreds
        let fec_group_size = 32usize;
        let mut code_shreds = Vec::new();
        let mut code_index = 0u32;

        for group_start in (0..data_shreds.len()).step_by(fec_group_size) {
            let group_end = (group_start + fec_group_size).min(data_shreds.len());
            let group = &data_shreds[group_start..group_end];
            let num_data = group.len();

            if num_data < 2 {
                continue;
            }

            let ec = ErasureCoder::from_fec_rate(num_data, FEC_RATE_PERCENT as u32);

            let shard_bytes: Vec<Vec<u8>> = group
                .iter()
                .map(|s| s.shred_bytes().to_vec())
                .collect();

            let max_len = shard_bytes.iter().map(|b| b.len()).max().unwrap_or(0);
            let padded: Vec<Vec<u8>> = shard_bytes
                .iter()
                .map(|b| {
                    let mut padded = b.clone();
                    padded.resize(max_len, 0);
                    padded
                })
                .collect();

            match ec.encode(&padded) {
                Ok(parity_shards) => {
                    for (j, parity) in parity_shards.iter().enumerate() {
                        let code = CodeShred {
                            slot,
                            index: code_index,
                            num_data_shreds: num_data as u32,
                            num_code_shreds: parity_shards.len() as u32,
                            position: j as u32,
                            data: parity.clone(),
                        };
                        code_shreds.push(MerkleCodeShred::new(code, leader));
                        code_index += 1;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "FEC encoding failed, skipping parity for group");
                }
            }
        }

        // Step 3: Hash ALL shreds (data first, then code) → build Merkle tree
        let mut all_hashes = Vec::with_capacity(data_shreds.len() + code_shreds.len());
        for s in &data_shreds {
            all_hashes.push(s.shred_hash());
        }
        for s in &code_shreds {
            all_hashes.push(s.shred_hash());
        }

        let tree = MerkleTree::new(&all_hashes);
        let merkle_root = tree.root();

        // Step 4: Attach proofs to each shred
        for (i, shred) in data_shreds.iter_mut().enumerate() {
            if let Some(proof) = tree.proof(i) {
                shred.merkle_proof = proof;
            }
        }
        for (j, shred) in code_shreds.iter_mut().enumerate() {
            let idx = data_shreds.len() + j;
            if let Some(proof) = tree.proof(idx) {
                shred.merkle_proof = proof;
            }
        }

        // Step 5: Build header with ONE signature over the root
        let header = build_batch_header(
            slot,
            leader,
            &data_shreds,
            &code_shreds,
            keypair,
            merkle_root,
        );

        metrics::counter!("nusantara_turbine_shreds_created_total")
            .increment((data_shreds.len() + code_shreds.len()) as u64);

        Ok(ShredBatch {
            header,
            data_shreds,
            code_shreds,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_core::block::{Block, BlockHeader};
    use nusantara_crypto::{Hash, hash};

    fn test_block(slot: u64, _tx_count: usize) -> Block {
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
            transactions: vec![],
            batches: Vec::new(),
        }
    }

    #[test]
    fn config_values() {
        assert_eq!(MAX_DATA_PER_SHRED, 1228);
        assert_eq!(FEC_RATE_PERCENT, 33);
    }

    #[test]
    fn shred_small_block() {
        let kp = Keypair::generate();
        let block = test_block(1, 0);
        let batch = Shredder::shred_block(&block, 0, &kp).unwrap();

        assert!(!batch.data_shreds.is_empty());
        let last = batch.data_shreds.last().unwrap();
        assert!(last.is_last());

        // All data shreds should have valid Merkle proofs
        for shred in &batch.data_shreds {
            assert!(shred.verify(&batch.header.merkle_root));
            assert_eq!(shred.slot(), 1);
        }

        // All code shreds should have valid Merkle proofs
        for shred in &batch.code_shreds {
            assert!(shred.verify(&batch.header.merkle_root));
        }

        // Header signature should verify
        assert!(batch.header.verify(kp.public_key()));
    }

    #[test]
    fn shred_indices_sequential() {
        let kp = Keypair::generate();
        let block = test_block(5, 0);
        let batch = Shredder::shred_block(&block, 4, &kp).unwrap();

        for (i, shred) in batch.data_shreds.iter().enumerate() {
            assert_eq!(shred.index(), i as u32);
        }
    }

    #[test]
    fn header_counts_match() {
        let kp = Keypair::generate();
        let block = test_block(1, 0);
        let batch = Shredder::shred_block(&block, 0, &kp).unwrap();

        assert_eq!(batch.header.num_data_shreds, batch.data_shreds.len() as u32);
        assert_eq!(batch.header.num_code_shreds, batch.code_shreds.len() as u32);
    }

    #[test]
    fn wrong_key_fails_header_verification() {
        let kp = Keypair::generate();
        let kp2 = Keypair::generate();
        let block = test_block(1, 0);
        let batch = Shredder::shred_block(&block, 0, &kp).unwrap();
        assert!(!batch.header.verify(kp2.public_key()));
    }
}
