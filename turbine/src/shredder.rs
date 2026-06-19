use nusantara_core::block::Block;
use nusantara_core::native_token::const_parse_u64;
use nusantara_crypto::{Keypair, MerkleTree};
use nusantara_storage::shred::{CodeShred, DataShred};

use crate::erasure::ErasureCoder;
use crate::error::TurbineError;
use crate::merkle_shred::{
    LAST_SHRED_FLAG, MerkleCodeShred, MerkleDataShred, ShredBatchHeader, build_batch_header,
};

pub const MAX_DATA_PER_SHRED: u64 = const_parse_u64(env!("NUSA_TURBINE_MAX_DATA_PER_SHRED"));
pub const FEC_RATE_PERCENT: u64 = const_parse_u64(env!("NUSA_TURBINE_FEC_RATE_PERCENT"));
pub const FEC_GROUP_SIZE: usize =
    const_parse_u64(env!("NUSA_TURBINE_FEC_GROUP_SIZE")) as usize;

pub struct ShredBatch {
    pub header: ShredBatchHeader,
    pub data_shreds: Vec<MerkleDataShred>,
    pub code_shreds: Vec<MerkleCodeShred>,
}

impl std::fmt::Debug for ShredBatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShredBatch")
            .field("slot", &self.header.slot)
            .field("data_shreds", &self.data_shreds.len())
            .field("code_shreds", &self.code_shreds.len())
            .finish()
    }
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

        // Validate parent_offset fits in u16 — silent zero would produce corrupt shreds.
        let parent_offset = if slot < parent_slot {
            return Err(TurbineError::BlockSerialization(
                "parent_slot > slot: invalid block ancestry".to_string(),
            ));
        } else {
            let diff = slot - parent_slot;
            u16::try_from(diff).map_err(|_| {
                TurbineError::BlockSerialization(format!(
                    "parent_offset overflow: slot={slot} parent_slot={parent_slot} diff={diff} > u16::MAX"
                ))
            })?
        };

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
                parent_offset,
                data: chunk.to_vec(),
                flags: if is_last { LAST_SHRED_FLAG } else { 0x00 },
            };
            data_shreds.push(MerkleDataShred::new(shred, leader)?);
        }

        // Step 2: FEC encode in groups of FEC_GROUP_SIZE data shreds.
        // Errors are propagated — skipping parity leaves the block unrecoverable on loss.
        //
        // All full groups share the same `num_data = FEC_GROUP_SIZE`; only the
        // final partial group may differ. Cache the ErasureCoder keyed by num_data
        // to avoid reconstructing Reed-Solomon GF tables on every iteration.
        let mut code_shreds = Vec::new();
        let mut code_index = 0u32;
        // (num_data, ErasureCoder) single-slot cache — avoids HashMap overhead
        // since all full groups have the same num_data.
        let mut ec_cache: Option<(usize, ErasureCoder)> = None;

        for group_start in (0..data_shreds.len()).step_by(FEC_GROUP_SIZE) {
            let group_end = (group_start + FEC_GROUP_SIZE).min(data_shreds.len());
            let group = &data_shreds[group_start..group_end];
            let num_data = group.len();

            if num_data < 2 {
                continue;
            }

            // Reuse cached ErasureCoder when num_data is unchanged (common case:
            // all full groups). Reconstruct only for the final partial group.
            // Use a single binding per arm — no double-borrow, no expect().
            if !matches!(&ec_cache, Some((n, _)) if *n == num_data) {
                ec_cache = Some((
                    num_data,
                    ErasureCoder::from_fec_rate(num_data, FEC_RATE_PERCENT as u32)?,
                ));
            }
            // INVARIANT: ec_cache is Some here — set either by the arm above or
            // because we matched the `if` condition (already correct num_data).
            let ec = &ec_cache
                .as_ref()
                .expect("ec_cache is Some: set unconditionally above this line")
                .1;

            // Build padded shards in one pass to avoid double-allocation.
            let max_len = group
                .iter()
                .map(|s| s.shred_bytes().map(|b| b.len()).unwrap_or(0))
                .max()
                .unwrap_or(0);

            let padded: Vec<Vec<u8>> = group
                .iter()
                .map(|s| -> Result<Vec<u8>, TurbineError> {
                    let raw = s.shred_bytes()?;
                    let mut buf = Vec::with_capacity(max_len);
                    buf.extend_from_slice(raw);
                    buf.resize(max_len, 0);
                    Ok(buf)
                })
                .collect::<Result<_, _>>()?;

            let parity_shards = ec.encode(&padded)?;

            for (j, parity) in parity_shards.iter().enumerate() {
                let code = CodeShred {
                    slot,
                    index: code_index,
                    num_data_shreds: num_data as u32,
                    num_code_shreds: parity_shards.len() as u32,
                    position: j as u32,
                    data: parity.clone(),
                };
                code_shreds.push(MerkleCodeShred::new(code, leader)?);
                code_index += 1;
            }
        }

        // Step 3: Hash ALL shreds (data first, then code) → build Merkle tree
        let mut all_hashes = Vec::with_capacity(data_shreds.len() + code_shreds.len());
        for s in &data_shreds {
            all_hashes.push(s.shred_hash()?);
        }
        for s in &code_shreds {
            all_hashes.push(s.shred_hash()?);
        }

        let tree = MerkleTree::new(&all_hashes);
        let merkle_root = tree.root();

        // Step 4: Attach proofs to each shred — missing proof is a logic error.
        for (i, shred) in data_shreds.iter_mut().enumerate() {
            shred.merkle_proof = tree.proof(i).ok_or_else(|| {
                TurbineError::Deshredding(format!("missing merkle proof for data shred {i}"))
            })?;
        }
        for (j, shred) in code_shreds.iter_mut().enumerate() {
            let idx = data_shreds.len() + j;
            shred.merkle_proof = tree.proof(idx).ok_or_else(|| {
                TurbineError::Deshredding(format!("missing merkle proof for code shred {j}"))
            })?;
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

    fn test_block(slot: u64, parent_slot: u64) -> Block {
        Block {
            header: BlockHeader {
                slot,
                parent_slot,
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
        assert_eq!(FEC_GROUP_SIZE, 32);
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
        let block = test_block(5, 4);
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

    #[test]
    fn parent_offset_overflow_returns_error() {
        let kp = Keypair::generate();
        // slot - parent_slot > u16::MAX
        let slot = 100_000u64;
        let parent_slot = 0u64;
        let block = test_block(slot, parent_slot);
        let result = Shredder::shred_block(&block, parent_slot, &kp);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("parent_offset overflow"), "got: {msg}");
    }

    #[test]
    fn parent_slot_greater_than_slot_returns_error() {
        let kp = Keypair::generate();
        let block = test_block(5, 3);
        // Pass parent_slot=10 > slot=5
        let result = Shredder::shred_block(&block, 10, &kp);
        assert!(result.is_err());
    }

    #[test]
    fn fec_group_size_from_config() {
        // Verify FEC_GROUP_SIZE constant is correctly read from config.toml
        assert_eq!(FEC_GROUP_SIZE, 32);
    }
}
