use borsh::{BorshDeserialize, BorshSerialize};

use crate::compression;
use crate::merkle_shred::{MerkleShred, ShredBatchHeader};

pub const MAX_UDP_PACKET: usize = 65507;

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
    pub fn pack(slot: u64, shreds: Vec<MerkleShred>, max_packet_size: usize) -> Vec<Self> {
        if shreds.is_empty() {
            return Vec::new();
        }

        let mut batches = Vec::new();
        let mut current = Vec::new();

        // Estimate overhead: TurbineMessage enum tag (1) + slot (8) + vec length prefix (4)
        let overhead = 13;

        let mut current_size = overhead;

        for shred in shreds {
            let shred_size = borsh::to_vec(&shred).map(|b| b.len()).unwrap_or(0);

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

        batches
    }
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
        let data_shred = MerkleDataShred::new(shred, kp.address());
        let hashes = vec![data_shred.shred_hash()];
        let tree = MerkleTree::new(&hashes);
        let proof = tree.proof(0).unwrap();
        let mut s = data_shred;
        s.merkle_proof = proof;
        MerkleShred::Data(s)
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
        let batches = BatchRepairResponse::pack(1, Vec::new(), MAX_UDP_PACKET);
        assert!(batches.is_empty());
    }

    #[test]
    fn batch_pack_single_shred() {
        let kp = Keypair::generate();
        let shred = make_merkle_shred(&kp, 1, 0, vec![0u8; 100]);

        let batches = BatchRepairResponse::pack(1, vec![shred], MAX_UDP_PACKET);
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
}
