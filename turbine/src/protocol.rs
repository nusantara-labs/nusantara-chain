use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_core::native_token::const_parse_u64;

use crate::compression;
use crate::error::TurbineError;
use crate::merkle_shred::{MerkleShred, ShredBatchHeader};

pub const MAX_UDP_PACKET: usize = 65507;

/// Maximum number of shreds accepted from a single `BatchRepairResponse` packet.
/// Prevents one packet from flooding the channel with unbounded channel sends.
pub const MAX_BATCH_RESPONSE_SHREDS: usize =
    const_parse_u64(env!("NUSA_TURBINE_MAX_BATCH_RESPONSE_SHREDS")) as usize;

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum TurbineMessage {
    ShredBatchHeader(ShredBatchHeader),
    Shred(MerkleShred),
    RepairRequest(RepairRequest),
    RepairResponse(MerkleShred),
    BatchRepairResponse(BatchRepairResponse),
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum RepairRequest {
    Shred { slot: u64, index: u32 },
    ShredBatch { slot: u64, indices: Vec<u32> },
    HighestShred { slot: u64 },
    Orphan { slot: u64 },
    BatchHeader { slot: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct BatchRepairResponse {
    pub slot: u64,
    pub shreds: Vec<MerkleShred>,
}

impl BatchRepairResponse {
    /// Greedily pack shreds into UDP-safe chunks.
    ///
    /// Returns an error if a single shred exceeds `max_packet_size` — such a
    /// shred can never fit and producing an oversize batch would corrupt the wire.
    pub fn pack(
        slot: u64,
        shreds: Vec<MerkleShred>,
        max_packet_size: usize,
    ) -> Result<Vec<Self>, TurbineError> {
        if shreds.is_empty() {
            return Ok(Vec::new());
        }

        let mut batches = Vec::new();
        let mut current: Vec<MerkleShred> = Vec::new();

        // Estimate overhead: TurbineMessage enum tag (1) + slot (8) + vec length prefix (4)
        let overhead = 13usize;
        let mut current_size = overhead;

        for shred in shreds {
            let shred_size = shred_wire_size(&shred);

            // A single shred that exceeds the MTU can never be packed — reject it
            // rather than silently producing an oversize batch.
            if shred_size > max_packet_size.saturating_sub(overhead) {
                return Err(TurbineError::ShredTooLarge {
                    size: shred_size + overhead,
                    max_size: max_packet_size,
                });
            }

            if !current.is_empty() && current_size + shred_size > max_packet_size {
                batches.push(BatchRepairResponse {
                    slot,
                    shreds: std::mem::take(&mut current),
                });
                current_size = overhead;
            }

            current_size += shred_size;
            current.push(shred);
        }

        if !current.is_empty() {
            batches.push(BatchRepairResponse {
                slot,
                shreds: current,
            });
        }

        Ok(batches)
    }
}

/// Compute the Borsh-serialized size of a `MerkleShred` without performing a
/// full serialization.
///
/// Layout for `MerkleShred::Data(MerkleDataShred)` (borsh is field-inline, no wrappers):
///   - 1 byte   : MerkleShred enum variant tag
///   - N bytes  : shred_bytes (the DataShred serialized inline — no extra length prefix;
///     MerkleDataShred::serialize writes the struct fields directly)
///   - 64 bytes : leader Hash (fixed 64-byte array)
///   - 4 bytes  : proof.hashes Vec length prefix
///   - M*64 bytes: proof hashes (each Hash is 64 bytes)
///   - 4 bytes  : proof.path Vec length prefix
///   - M bytes  : proof path entries (each is 1 byte, a u8 direction)
///
/// The same layout applies to `MerkleShred::Code`.
///
/// A test verifies this matches the actual `borsh::to_vec` output size.
fn shred_wire_size(shred: &MerkleShred) -> usize {
    let (shred_bytes_len, proof) = match shred {
        MerkleShred::Data(s) => (
            s.shred_bytes().map(|b| b.len()).unwrap_or(0),
            &s.merkle_proof,
        ),
        MerkleShred::Code(s) => (
            s.shred_bytes().map(|b| b.len()).unwrap_or(0),
            &s.merkle_proof,
        ),
    };

    let proof_hashes_bytes = 4 + proof.hashes.len() * 64; // 4-byte len prefix + 64 bytes per Hash
    let proof_path_bytes = 4 + proof.path.len(); // 4-byte len prefix + 1 byte per path step

    1                   // MerkleShred enum tag
    + shred_bytes_len   // inline struct bytes (no extra Vec length prefix)
    + 64                // leader Hash (fixed 64 bytes)
    + proof_hashes_bytes
    + proof_path_bytes
}

impl TurbineMessage {
    pub fn serialize_to_bytes(&self) -> Result<Vec<u8>, String> {
        let raw = borsh::to_vec(self).map_err(|e| e.to_string())?;
        compression::compress(&raw).map_err(|e| e.to_string())
    }

    pub fn deserialize_from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let raw = compression::decompress(bytes).map_err(|e| e.to_string())?;
        borsh::from_slice(&raw).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::{Keypair, MerkleTree, hash};
    use nusantara_storage::shred::DataShred;
    use crate::merkle_shred::MerkleDataShred;

    fn make_merkle_shred(kp: &Keypair, slot: u64, index: u32, data: Vec<u8>) -> MerkleShred {
        let shred = DataShred {
            slot,
            index,
            parent_offset: 1,
            data,
            flags: 0,
        };
        let mut data_shred = MerkleDataShred::new(shred, kp.address()).unwrap();
        let hashes = vec![data_shred.shred_hash().unwrap()];
        let tree = MerkleTree::new(&hashes);
        let proof = tree.proof(0).unwrap();
        data_shred.merkle_proof = proof;
        MerkleShred::Data(data_shred)
    }

    #[test]
    fn shred_message_roundtrip() {
        let kp = Keypair::generate();
        let shred = make_merkle_shred(&kp, 1, 0, vec![42u8; 100]);
        let msg = TurbineMessage::Shred(shred);

        let bytes = msg.serialize_to_bytes().unwrap();
        let decoded = TurbineMessage::deserialize_from_bytes(&bytes).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn repair_request_roundtrip() {
        let msg = TurbineMessage::RepairRequest(RepairRequest::Shred {
            slot: 10,
            index: 5,
        });
        let bytes = msg.serialize_to_bytes().unwrap();
        let decoded = TurbineMessage::deserialize_from_bytes(&bytes).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn batch_header_request_roundtrip() {
        let msg = TurbineMessage::RepairRequest(RepairRequest::BatchHeader { slot: 42 });
        let bytes = msg.serialize_to_bytes().unwrap();
        let decoded = TurbineMessage::deserialize_from_bytes(&bytes).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn shred_batch_header_message_roundtrip() {
        let kp = Keypair::generate();
        let root = hash(b"test_root");
        let sig = kp.sign(root.as_bytes());
        let header = ShredBatchHeader {
            slot: 5,
            leader: kp.address(),
            merkle_root: root,
            signature: sig,
            num_data_shreds: 10,
            num_code_shreds: 3,
        };
        let msg = TurbineMessage::ShredBatchHeader(header);
        let bytes = msg.serialize_to_bytes().unwrap();
        let decoded = TurbineMessage::deserialize_from_bytes(&bytes).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn batch_repair_response_roundtrip() {
        let kp = Keypair::generate();
        let shreds: Vec<MerkleShred> = (0..3)
            .map(|i| make_merkle_shred(&kp, 5, i, vec![i as u8; 100]))
            .collect();

        let msg = TurbineMessage::BatchRepairResponse(BatchRepairResponse {
            slot: 5,
            shreds,
        });
        let bytes = msg.serialize_to_bytes().unwrap();
        let decoded = TurbineMessage::deserialize_from_bytes(&bytes).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn batch_pack_empty() {
        let batches = BatchRepairResponse::pack(1, Vec::new(), MAX_UDP_PACKET).unwrap();
        assert!(batches.is_empty());
    }

    #[test]
    fn batch_pack_single_shred() {
        let kp = Keypair::generate();
        let shred = make_merkle_shred(&kp, 1, 0, vec![0u8; 100]);

        let batches = BatchRepairResponse::pack(1, vec![shred], MAX_UDP_PACKET).unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].shreds.len(), 1);
    }

    #[test]
    fn compression_roundtrip_large_message() {
        let kp = Keypair::generate();
        // Create a large shred that will trigger compression
        let shred = make_merkle_shred(&kp, 1, 0, vec![0u8; 1000]);
        let msg = TurbineMessage::Shred(shred);

        let bytes = msg.serialize_to_bytes().unwrap();
        let decoded = TurbineMessage::deserialize_from_bytes(&bytes).unwrap();
        assert_eq!(msg, decoded);
    }

    /// Verify `shred_wire_size` matches the actual `borsh::to_vec` size.
    #[test]
    fn shred_wire_size_matches_borsh() {
        let kp = Keypair::generate();
        // Test with data shred of various sizes
        for data_len in [0usize, 10, 100, 1000, 1228] {
            let shred = make_merkle_shred(&kp, 1, 0, vec![0xAB; data_len]);
            let actual = borsh::to_vec(&shred).unwrap().len();
            let computed = shred_wire_size(&shred);
            assert_eq!(
                computed,
                actual,
                "shred_wire_size mismatch for data_len={data_len}: computed={computed} actual={actual}"
            );
        }
    }

    #[test]
    fn pack_rejects_oversize_single_shred() {
        let kp = Keypair::generate();
        // Create a shred that is large enough to exceed a tiny max_packet_size
        let shred = make_merkle_shred(&kp, 1, 0, vec![0u8; 1000]);
        // Set max_packet_size smaller than any shred can fit
        let result = BatchRepairResponse::pack(1, vec![shred], 50);
        assert!(result.is_err());
        match result.unwrap_err() {
            TurbineError::ShredTooLarge { .. } => {}
            e => panic!("expected ShredTooLarge, got: {e}"),
        }
    }

    #[test]
    fn max_batch_response_shreds_constant() {
        assert_eq!(MAX_BATCH_RESPONSE_SHREDS, 64);
    }
}
