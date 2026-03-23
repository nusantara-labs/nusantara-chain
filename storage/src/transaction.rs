use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::Hash;
use rocksdb::IteratorMode;

use crate::cf::{CF_ADDRESS_SIGNATURES, CF_TRANSACTIONS};
use crate::error::StorageError;
use crate::keys::address_sig_key;
use crate::storage::Storage;

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct TransactionStatusMeta {
    pub slot: u64,
    pub status: TransactionStatus,
    pub fee: u64,
    pub pre_balances: Vec<u64>,
    pub post_balances: Vec<u64>,
    pub compute_units_consumed: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum TransactionStatus {
    Success,
    Failed(String),
}

impl Storage {
    /// Store a transaction status.
    #[tracing::instrument(skip(self, meta), fields(tx_hash = %tx_hash), level = "debug")]
    pub fn put_transaction_status(
        &self,
        tx_hash: &Hash,
        meta: &TransactionStatusMeta,
    ) -> Result<(), StorageError> {
        let value =
            borsh::to_vec(meta).map_err(|e| StorageError::Serialization(e.to_string()))?;
        self.put_cf(CF_TRANSACTIONS, tx_hash.as_bytes(), &value)
    }

    /// Prepare a transaction status write into a `StorageWriteBatch` without committing.
    pub fn prepare_transaction_status(
        tx_hash: &Hash,
        meta: &TransactionStatusMeta,
    ) -> Result<crate::write_batch::StorageWriteBatch, StorageError> {
        let mut batch = crate::write_batch::StorageWriteBatch::new();
        Self::append_transaction_status(&mut batch, tx_hash, meta)?;
        Ok(batch)
    }

    /// Append a transaction status write directly into the caller's batch.
    pub fn append_transaction_status(
        batch: &mut crate::write_batch::StorageWriteBatch,
        tx_hash: &Hash,
        meta: &TransactionStatusMeta,
    ) -> Result<(), StorageError> {
        let value =
            borsh::to_vec(meta).map_err(|e| StorageError::Serialization(e.to_string()))?;
        batch.put(CF_TRANSACTIONS, tx_hash.as_bytes().to_vec(), value);
        Ok(())
    }

    /// Prepare an address-signature write into a `StorageWriteBatch` without committing.
    pub fn prepare_address_signature(
        address: &Hash,
        slot: u64,
        tx_index: u32,
        tx_hash: &Hash,
    ) -> crate::write_batch::StorageWriteBatch {
        let mut batch = crate::write_batch::StorageWriteBatch::new();
        Self::append_address_signature(&mut batch, address, slot, tx_index, tx_hash);
        batch
    }

    /// Append an address-signature write directly into the caller's batch.
    pub fn append_address_signature(
        batch: &mut crate::write_batch::StorageWriteBatch,
        address: &Hash,
        slot: u64,
        tx_index: u32,
        tx_hash: &Hash,
    ) {
        let key = address_sig_key(address, slot, tx_index);
        batch.put(CF_ADDRESS_SIGNATURES, key.to_vec(), tx_hash.as_bytes().to_vec());
    }

    /// Get a transaction status by hash.
    pub fn get_transaction_status(
        &self,
        tx_hash: &Hash,
    ) -> Result<Option<TransactionStatusMeta>, StorageError> {
        match self.get_cf(CF_TRANSACTIONS, tx_hash.as_bytes())? {
            Some(bytes) => {
                let meta = TransactionStatusMeta::try_from_slice(&bytes)
                    .map_err(|e| StorageError::Deserialization(e.to_string()))?;
                Ok(Some(meta))
            }
            None => Ok(None),
        }
    }

    /// Store an address-to-transaction mapping.
    pub fn put_address_signature(
        &self,
        address: &Hash,
        slot: u64,
        tx_index: u32,
        tx_hash: &Hash,
    ) -> Result<(), StorageError> {
        let key = address_sig_key(address, slot, tx_index);
        self.put_cf(CF_ADDRESS_SIGNATURES, &key, tx_hash.as_bytes())
    }

    /// Get transaction signatures for an address, ordered by (slot, tx_index) descending.
    /// Returns `(slot, tx_index, tx_hash)` tuples.
    #[tracing::instrument(skip(self), fields(address = %address), level = "debug")]
    pub fn get_signatures_for_address(
        &self,
        address: &Hash,
        limit: usize,
    ) -> Result<Vec<(u64, u32, Hash)>, StorageError> {
        let cf = self
            .db
            .cf_handle(CF_ADDRESS_SIGNATURES)
            .ok_or(StorageError::CfNotFound(CF_ADDRESS_SIGNATURES))?;

        let prefix = address.as_bytes();
        let end_key = address_sig_key(address, u64::MAX, u32::MAX);
        let iter = self.db.iterator_cf(
            cf,
            IteratorMode::From(&end_key, rocksdb::Direction::Reverse),
        );

        let mut results = Vec::new();
        for item in iter {
            let (key, value) = item.map_err(StorageError::RocksDb)?;
            if key.len() < 76 || &key[..64] != prefix {
                break;
            }
            let slot = u64::from_be_bytes(
                key[64..72]
                    .try_into()
                    .map_err(|_| StorageError::Corruption("invalid slot bytes".into()))?,
            );
            let tx_index = u32::from_be_bytes(
                key[72..76]
                    .try_into()
                    .map_err(|_| StorageError::Corruption("invalid tx_index bytes".into()))?,
            );
            let tx_hash_bytes: [u8; 64] = value
                .as_ref()
                .try_into()
                .map_err(|_| StorageError::Corruption("invalid tx hash in value".into()))?;
            let tx_hash = Hash::new(tx_hash_bytes);
            results.push((slot, tx_index, tx_hash));
            if results.len() >= limit {
                break;
            }
        }
        Ok(results)
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

    fn test_meta(slot: u64) -> TransactionStatusMeta {
        TransactionStatusMeta {
            slot,
            status: TransactionStatus::Success,
            fee: 5000,
            pre_balances: vec![1000, 2000],
            post_balances: vec![995, 2005],
            compute_units_consumed: 200,
        }
    }

    #[test]
    fn put_and_get_transaction_status() {
        let (storage, _dir) = temp_storage();
        let tx_hash = hash(b"tx1");
        let meta = test_meta(1);

        storage.put_transaction_status(&tx_hash, &meta).unwrap();
        let loaded = storage.get_transaction_status(&tx_hash).unwrap().unwrap();
        assert_eq!(loaded, meta);
    }

    #[test]
    fn get_missing_transaction_status() {
        let (storage, _dir) = temp_storage();
        let tx_hash = hash(b"missing");
        assert_eq!(storage.get_transaction_status(&tx_hash).unwrap(), None);
    }

    #[test]
    fn transaction_status_failed() {
        let (storage, _dir) = temp_storage();
        let tx_hash = hash(b"failed_tx");
        let meta = TransactionStatusMeta {
            slot: 1,
            status: TransactionStatus::Failed("insufficient funds".into()),
            fee: 5000,
            pre_balances: vec![100],
            post_balances: vec![100],
            compute_units_consumed: 50,
        };

        storage.put_transaction_status(&tx_hash, &meta).unwrap();
        let loaded = storage.get_transaction_status(&tx_hash).unwrap().unwrap();
        assert_eq!(loaded, meta);
    }

    #[test]
    fn address_signatures() {
        let (storage, _dir) = temp_storage();
        let addr = hash(b"alice");
        let tx1 = hash(b"tx1");
        let tx2 = hash(b"tx2");
        let tx3 = hash(b"tx3");

        storage.put_address_signature(&addr, 1, 0, &tx1).unwrap();
        storage.put_address_signature(&addr, 1, 1, &tx2).unwrap();
        storage.put_address_signature(&addr, 5, 0, &tx3).unwrap();

        let sigs = storage.get_signatures_for_address(&addr, 10).unwrap();
        assert_eq!(sigs.len(), 3);
        // Most recent first (slot 5, then slot 1 tx_index 1, then slot 1 tx_index 0)
        assert_eq!(sigs[0], (5, 0, tx3));
        assert_eq!(sigs[1], (1, 1, tx2));
        assert_eq!(sigs[2], (1, 0, tx1));
    }

    #[test]
    fn address_signatures_limit() {
        let (storage, _dir) = temp_storage();
        let addr = hash(b"bob");

        for i in 0..10u32 {
            let tx = hash(format!("tx_{i}").as_bytes());
            storage.put_address_signature(&addr, 1, i, &tx).unwrap();
        }

        let sigs = storage.get_signatures_for_address(&addr, 3).unwrap();
        assert_eq!(sigs.len(), 3);
    }

    #[test]
    fn address_signatures_isolated() {
        let (storage, _dir) = temp_storage();
        let addr1 = hash(b"user1");
        let addr2 = hash(b"user2");
        let tx1 = hash(b"tx_a");
        let tx2 = hash(b"tx_b");

        storage.put_address_signature(&addr1, 1, 0, &tx1).unwrap();
        storage.put_address_signature(&addr2, 1, 0, &tx2).unwrap();

        let sigs1 = storage.get_signatures_for_address(&addr1, 10).unwrap();
        assert_eq!(sigs1.len(), 1);
        assert_eq!(sigs1[0].2, tx1);

        let sigs2 = storage.get_signatures_for_address(&addr2, 10).unwrap();
        assert_eq!(sigs2.len(), 1);
        assert_eq!(sigs2[0].2, tx2);
    }

    #[test]
    fn borsh_roundtrip_status_meta() {
        let meta = test_meta(42);
        let encoded = borsh::to_vec(&meta).unwrap();
        let decoded: TransactionStatusMeta = borsh::from_slice(&encoded).unwrap();
        assert_eq!(meta, decoded);
    }
}
