use std::sync::OnceLock;

use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::{Hash, Keypair, MerkleProof, PublicKey, Signature, hash as crypto_hash};
use nusantara_storage::shred::{CodeShred, DataShred};

use crate::error::TurbineError;

/// Flag set on the last data shred of a slot.
/// Shared constant used by shredder, deshredder, and collector.
pub const LAST_SHRED_FLAG: u8 = 0x01;

/// Sent once per slot — contains the Merkle root signed by the leader.
#[derive(Clone, Debug)]
pub struct ShredBatchHeader {
    pub slot: u64,
    pub leader: Hash,
    pub merkle_root: Hash,
    pub signature: Signature,
    pub num_data_shreds: u32,
    pub num_code_shreds: u32,
}

impl PartialEq for ShredBatchHeader {
    fn eq(&self, other: &Self) -> bool {
        self.slot == other.slot
            && self.leader == other.leader
            && self.merkle_root == other.merkle_root
            && self.signature == other.signature
            && self.num_data_shreds == other.num_data_shreds
            && self.num_code_shreds == other.num_code_shreds
    }
}

impl Eq for ShredBatchHeader {}

impl BorshSerialize for ShredBatchHeader {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        BorshSerialize::serialize(&self.slot, writer)?;
        BorshSerialize::serialize(&self.leader, writer)?;
        BorshSerialize::serialize(&self.merkle_root, writer)?;
        BorshSerialize::serialize(&self.signature, writer)?;
        BorshSerialize::serialize(&self.num_data_shreds, writer)?;
        BorshSerialize::serialize(&self.num_code_shreds, writer)?;
        Ok(())
    }
}

impl BorshDeserialize for ShredBatchHeader {
    fn deserialize_reader<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        let slot = u64::deserialize_reader(reader)?;
        let leader = Hash::deserialize_reader(reader)?;
        let merkle_root = Hash::deserialize_reader(reader)?;
        let signature = Signature::deserialize_reader(reader)?;
        let num_data_shreds = u32::deserialize_reader(reader)?;
        let num_code_shreds = u32::deserialize_reader(reader)?;
        Ok(Self {
            slot,
            leader,
            merkle_root,
            signature,
            num_data_shreds,
            num_code_shreds,
        })
    }
}

impl ShredBatchHeader {
    /// Verify the signature over the Merkle root.
    pub fn verify(&self, pubkey: &PublicKey) -> bool {
        self.signature
            .verify(pubkey, self.merkle_root.as_bytes())
            .is_ok()
    }
}

/// Data shred with a Merkle proof instead of a full Dilithium3 signature.
///
/// # Cache invariant
/// `cached_bytes` is a `OnceLock` populated lazily by `shred_bytes()` and
/// `shred_hash()`. The `shred` field is `pub(crate)` to prevent external
/// mutation that would desync the cache. If you need to produce a modified
/// shred, construct a new `MerkleDataShred` via `new()`.
#[derive(Debug)]
pub struct MerkleDataShred {
    pub(crate) shred: DataShred,
    pub leader: Hash,
    pub merkle_proof: MerkleProof,
    /// Lazily populated Borsh serialization of `shred`. Not serialized on the wire.
    cached_bytes: OnceLock<Vec<u8>>,
}

impl Clone for MerkleDataShred {
    fn clone(&self) -> Self {
        Self {
            shred: self.shred.clone(),
            leader: self.leader,
            merkle_proof: self.merkle_proof.clone(),
            // Clone the cached bytes if already populated, otherwise leave empty
            // so the clone's OnceLock is re-populated lazily on first access.
            cached_bytes: self
                .cached_bytes
                .get()
                .map(|b| {
                    let cell = OnceLock::new();
                    let _ = cell.set(b.clone());
                    cell
                })
                .unwrap_or_default(),
        }
    }
}

impl PartialEq for MerkleDataShred {
    fn eq(&self, other: &Self) -> bool {
        self.shred == other.shred
            && self.leader == other.leader
            && self.merkle_proof == other.merkle_proof
    }
}

impl Eq for MerkleDataShred {}

impl BorshSerialize for MerkleDataShred {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        BorshSerialize::serialize(&self.shred, writer)?;
        BorshSerialize::serialize(&self.leader, writer)?;
        BorshSerialize::serialize(&self.merkle_proof, writer)?;
        Ok(())
    }
}

impl BorshDeserialize for MerkleDataShred {
    fn deserialize_reader<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        let shred = DataShred::deserialize_reader(reader)?;
        let leader = Hash::deserialize_reader(reader)?;
        let merkle_proof = MerkleProof::deserialize_reader(reader)?;
        // cached_bytes populated lazily on first access — no re-serialization here.
        Ok(Self {
            shred,
            leader,
            merkle_proof,
            cached_bytes: OnceLock::new(),
        })
    }
}

impl MerkleDataShred {
    /// Create a new MerkleDataShred without a proof yet (proof attached later).
    ///
    /// Pre-serializes the inner `DataShred` at construction time so that
    /// `shred_bytes()` is infallible on the hot path (FEC encode, Merkle hash).
    /// Returns `Err` if Borsh serialization fails — this surfaces the error at
    /// the call site (shredder) rather than panicking inside a hot-path closure.
    pub fn new(shred: DataShred, leader: Hash) -> Result<Self, TurbineError> {
        let bytes = borsh::to_vec(&shred)
            .map_err(|e| TurbineError::Borsh(e.to_string()))?;
        let cached_bytes = OnceLock::new();
        // Infallible: we just created the cell and it is empty.
        let _ = cached_bytes.set(bytes);
        Ok(Self {
            shred,
            leader,
            merkle_proof: MerkleProof {
                hashes: Vec::new(),
                path: Vec::new(),
            },
            cached_bytes,
        })
    }

    /// Hash of this shred — the leaf value for Merkle tree construction.
    ///
    /// Propagates any serialization error from the lazy-populated cache
    /// (only reachable for deserialized shreds whose inner bytes were never
    /// pre-computed).
    pub fn shred_hash(&self) -> Result<Hash, TurbineError> {
        Ok(crypto_hash(self.shred_bytes()?))
    }

    /// Verify the Merkle proof against a known root.
    /// Returns `false` on serialization failure (treats it as invalid).
    pub fn verify(&self, merkle_root: &Hash) -> bool {
        match self.shred_hash() {
            Ok(h) => self.merkle_proof.verify(&h, merkle_root),
            Err(_) => false,
        }
    }

    /// Access the cached serialized shred bytes (for FEC encoding).
    ///
    /// For instances created via `new()`, the bytes are pre-populated and this
    /// is a simple pointer return. For instances created via `BorshDeserialize`,
    /// the bytes are computed lazily on first call.
    ///
    /// Returns `Err` only on the lazy-path if Borsh serialization fails.
    pub fn shred_bytes(&self) -> Result<&[u8], TurbineError> {
        // Fast path: already populated (new() or prior lazy call).
        if let Some(b) = self.cached_bytes.get() {
            return Ok(b);
        }
        // Lazy path: deserialized shred — compute and cache.
        let bytes = borsh::to_vec(&self.shred)
            .map_err(|e| TurbineError::Borsh(e.to_string()))?;
        // Another thread may have raced us; get() after set() returns whichever won.
        let _ = self.cached_bytes.set(bytes);
        Ok(self.cached_bytes.get().expect("just set above or already present"))
    }

    pub fn slot(&self) -> u64 {
        self.shred.slot
    }

    pub fn index(&self) -> u32 {
        self.shred.index
    }

    pub fn is_last(&self) -> bool {
        self.shred.flags & LAST_SHRED_FLAG != 0
    }
}

/// Code shred with a Merkle proof instead of a full Dilithium3 signature.
///
/// # Cache invariant
/// Same as `MerkleDataShred` — `shred` is `pub(crate)`, `cached_bytes` is lazy.
#[derive(Debug)]
pub struct MerkleCodeShred {
    pub(crate) shred: CodeShred,
    pub leader: Hash,
    pub merkle_proof: MerkleProof,
    cached_bytes: OnceLock<Vec<u8>>,
}

impl Clone for MerkleCodeShred {
    fn clone(&self) -> Self {
        Self {
            shred: self.shred.clone(),
            leader: self.leader,
            merkle_proof: self.merkle_proof.clone(),
            cached_bytes: self
                .cached_bytes
                .get()
                .map(|b| {
                    let cell = OnceLock::new();
                    let _ = cell.set(b.clone());
                    cell
                })
                .unwrap_or_default(),
        }
    }
}

impl PartialEq for MerkleCodeShred {
    fn eq(&self, other: &Self) -> bool {
        self.shred == other.shred
            && self.leader == other.leader
            && self.merkle_proof == other.merkle_proof
    }
}

impl Eq for MerkleCodeShred {}

impl BorshSerialize for MerkleCodeShred {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        BorshSerialize::serialize(&self.shred, writer)?;
        BorshSerialize::serialize(&self.leader, writer)?;
        BorshSerialize::serialize(&self.merkle_proof, writer)?;
        Ok(())
    }
}

impl BorshDeserialize for MerkleCodeShred {
    fn deserialize_reader<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        let shred = CodeShred::deserialize_reader(reader)?;
        let leader = Hash::deserialize_reader(reader)?;
        let merkle_proof = MerkleProof::deserialize_reader(reader)?;
        Ok(Self {
            shred,
            leader,
            merkle_proof,
            cached_bytes: OnceLock::new(),
        })
    }
}

impl MerkleCodeShred {
    /// Pre-serializes the inner `CodeShred` at construction time.
    /// Returns `Err` if Borsh serialization fails.
    pub fn new(shred: CodeShred, leader: Hash) -> Result<Self, TurbineError> {
        let bytes = borsh::to_vec(&shred)
            .map_err(|e| TurbineError::Borsh(e.to_string()))?;
        let cached_bytes = OnceLock::new();
        let _ = cached_bytes.set(bytes);
        Ok(Self {
            shred,
            leader,
            merkle_proof: MerkleProof {
                hashes: Vec::new(),
                path: Vec::new(),
            },
            cached_bytes,
        })
    }

    pub fn shred_hash(&self) -> Result<Hash, TurbineError> {
        Ok(crypto_hash(self.shred_bytes()?))
    }

    pub fn verify(&self, merkle_root: &Hash) -> bool {
        match self.shred_hash() {
            Ok(h) => self.merkle_proof.verify(&h, merkle_root),
            Err(_) => false,
        }
    }

    pub fn shred_bytes(&self) -> Result<&[u8], TurbineError> {
        if let Some(b) = self.cached_bytes.get() {
            return Ok(b);
        }
        let bytes = borsh::to_vec(&self.shred)
            .map_err(|e| TurbineError::Borsh(e.to_string()))?;
        let _ = self.cached_bytes.set(bytes);
        Ok(self.cached_bytes.get().expect("just set above or already present"))
    }

    pub fn slot(&self) -> u64 {
        self.shred.slot
    }

    pub fn index(&self) -> u32 {
        self.shred.index
    }
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum MerkleShred {
    Data(MerkleDataShred),
    Code(MerkleCodeShred),
}

impl MerkleShred {
    pub fn slot(&self) -> u64 {
        match self {
            Self::Data(s) => s.slot(),
            Self::Code(s) => s.slot(),
        }
    }

    pub fn index(&self) -> u32 {
        match self {
            Self::Data(s) => s.index(),
            Self::Code(s) => s.index(),
        }
    }

    pub fn leader(&self) -> Hash {
        match self {
            Self::Data(s) => s.leader,
            Self::Code(s) => s.leader,
        }
    }

    /// Verify the Merkle proof against a known root.
    /// Returns `false` on serialization failure (treats corrupted shred as invalid).
    pub fn verify(&self, merkle_root: &Hash) -> bool {
        match self {
            Self::Data(s) => s.verify(merkle_root),
            Self::Code(s) => s.verify(merkle_root),
        }
    }
}

/// Build a ShredBatchHeader by signing the Merkle root of all shred hashes.
pub fn build_batch_header(
    slot: u64,
    leader: Hash,
    data_shreds: &[MerkleDataShred],
    code_shreds: &[MerkleCodeShred],
    keypair: &Keypair,
    merkle_root: Hash,
) -> ShredBatchHeader {
    let signature = keypair.sign(merkle_root.as_bytes());
    ShredBatchHeader {
        slot,
        leader,
        merkle_root,
        signature,
        num_data_shreds: data_shreds.len() as u32,
        num_code_shreds: code_shreds.len() as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::{Keypair, MerkleTree, hash};

    #[test]
    fn data_shred_borsh_roundtrip() {
        let shred = DataShred {
            slot: 5,
            index: 3,
            parent_offset: 1,
            data: vec![99u8; 50],
            flags: LAST_SHRED_FLAG,
        };
        let leader = hash(b"leader");
        let merkle_data = MerkleDataShred::new(shred, leader).unwrap();

        let bytes = borsh::to_vec(&merkle_data).unwrap();
        let decoded: MerkleDataShred = borsh::from_slice(&bytes).unwrap();
        assert_eq!(merkle_data, decoded);
        assert_eq!(merkle_data.shred_hash().unwrap(), decoded.shred_hash().unwrap());
    }

    #[test]
    fn code_shred_borsh_roundtrip() {
        let shred = CodeShred {
            slot: 1,
            index: 0,
            num_data_shreds: 10,
            num_code_shreds: 4,
            position: 0,
            data: vec![0xAB; 100],
        };
        let leader = hash(b"leader");
        let merkle_code = MerkleCodeShred::new(shred, leader).unwrap();

        let bytes = borsh::to_vec(&merkle_code).unwrap();
        let decoded: MerkleCodeShred = borsh::from_slice(&bytes).unwrap();
        assert_eq!(merkle_code, decoded);
    }

    #[test]
    fn proof_verification() {
        let kp = Keypair::generate();
        let shred = DataShred {
            slot: 1,
            index: 0,
            parent_offset: 1,
            data: vec![42u8; 100],
            flags: 0,
        };
        let mut merkle_data = MerkleDataShred::new(shred, kp.address()).unwrap();
        let leaf_hash = merkle_data.shred_hash().unwrap();

        let tree = MerkleTree::new(&[leaf_hash]);
        let proof = tree.proof(0).unwrap();
        merkle_data.merkle_proof = proof;

        assert!(merkle_data.verify(&tree.root()));
    }

    #[test]
    fn wrong_root_fails_verification() {
        let kp = Keypair::generate();
        let shred = DataShred {
            slot: 1,
            index: 0,
            parent_offset: 1,
            data: vec![42u8; 100],
            flags: 0,
        };
        let mut merkle_data = MerkleDataShred::new(shred, kp.address()).unwrap();
        let leaf_hash = merkle_data.shred_hash().unwrap();

        let tree = MerkleTree::new(&[leaf_hash]);
        let proof = tree.proof(0).unwrap();
        merkle_data.merkle_proof = proof;

        let wrong_root = hash(b"wrong");
        assert!(!merkle_data.verify(&wrong_root));
    }

    #[test]
    fn shred_hash_determinism() {
        let shred = DataShred {
            slot: 1,
            index: 0,
            parent_offset: 1,
            data: vec![42u8; 100],
            flags: 0,
        };
        let leader = hash(b"leader");
        let a = MerkleDataShred::new(shred.clone(), leader).unwrap();
        let b = MerkleDataShred::new(shred, leader).unwrap();
        assert_eq!(a.shred_hash().unwrap(), b.shred_hash().unwrap());
    }

    #[test]
    fn batch_header_verify() {
        let kp = Keypair::generate();
        let root = hash(b"merkle_root");
        let header = ShredBatchHeader {
            slot: 1,
            leader: kp.address(),
            merkle_root: root,
            signature: kp.sign(root.as_bytes()),
            num_data_shreds: 5,
            num_code_shreds: 2,
        };
        assert!(header.verify(kp.public_key()));

        let kp2 = Keypair::generate();
        assert!(!header.verify(kp2.public_key()));
    }

    #[test]
    fn merkle_shred_enum_roundtrip() {
        let shred = DataShred {
            slot: 5,
            index: 3,
            parent_offset: 1,
            data: vec![99u8; 50],
            flags: LAST_SHRED_FLAG,
        };
        let leader = hash(b"leader");
        let merkle_data = MerkleDataShred::new(shred, leader).unwrap();
        let ms = MerkleShred::Data(merkle_data);
        let bytes = borsh::to_vec(&ms).unwrap();
        let decoded: MerkleShred = borsh::from_slice(&bytes).unwrap();
        assert_eq!(ms, decoded);
    }

    #[test]
    fn cached_bytes_lazy_population() {
        let shred = DataShred {
            slot: 1,
            index: 0,
            parent_offset: 0,
            data: vec![1, 2, 3],
            flags: 0,
        };
        let ms = MerkleDataShred::new(shred.clone(), hash(b"leader")).unwrap();
        // First access returns pre-populated cache (from new())
        let b1 = ms.shred_bytes().unwrap().to_vec();
        // Second access returns same cached bytes
        let b2 = ms.shred_bytes().unwrap().to_vec();
        assert_eq!(b1, b2);
        // Verify the bytes match manual serialization
        let expected = borsh::to_vec(&shred).unwrap();
        assert_eq!(b1, expected);
    }

    #[test]
    fn deserialized_shred_cache_correct() {
        let shred = DataShred {
            slot: 7,
            index: 2,
            parent_offset: 1,
            data: vec![0xAB; 64],
            flags: 0,
        };
        let original = MerkleDataShred::new(shred, hash(b"leader")).unwrap();
        let bytes = borsh::to_vec(&original).unwrap();
        let decoded: MerkleDataShred = borsh::from_slice(&bytes).unwrap();
        // Cache must produce the same hash as the original
        assert_eq!(original.shred_hash().unwrap(), decoded.shred_hash().unwrap());
    }
}
