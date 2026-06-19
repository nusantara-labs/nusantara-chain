pub mod error;
pub mod cf;
pub mod keys;
pub mod write_batch;
pub mod storage;
pub mod account_index;
pub mod owner_index;
pub mod block;
pub mod transaction;
pub mod slot_meta;
pub mod shred;
pub mod bank;
pub mod snapshot;
pub mod sysvar;
pub mod snapshot_archive;
pub mod pruning;
pub mod slashing;

pub use error::StorageError;
pub use storage::Storage;
pub use write_batch::StorageWriteBatch;
pub use slot_meta::SlotMeta;
pub use shred::{DataShred, CodeShred};
pub use transaction::{TransactionStatusMeta, TransactionStatus};
pub use snapshot::SnapshotManifest;
pub use slashing::SlashProof;

/// Decode borsh-serialized bytes into `T` using the project-standard pattern:
/// `T::deserialize(&mut &*bytes)`. This avoids the `try_from_slice` pattern
/// which may silently succeed on truncated data when `T` has trailing fields.
pub(crate) fn decode<T: borsh::BorshDeserialize>(bytes: &[u8]) -> Result<T, StorageError> {
    T::deserialize(&mut &*bytes).map_err(|e| StorageError::Deserialization(e.to_string()))
}
