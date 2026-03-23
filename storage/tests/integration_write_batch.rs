use nusantara_core::Account;
use nusantara_crypto::hash;
use nusantara_storage::{Storage, StorageWriteBatch};

fn temp_storage() -> (Storage, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();
    (storage, dir)
}

#[test]
fn batch_multi_cf_atomic_write() {
    let (storage, _dir) = temp_storage();
    let addr = hash(b"alice");
    let account = Account::new(1000, hash(b"system"));

    // Use write batch to atomically write account + bank hash + root
    let mut batch = StorageWriteBatch::new();

    // Account entry
    let acc_key = {
        let mut k = [0u8; 72];
        k[..64].copy_from_slice(addr.as_bytes());
        k[64..].copy_from_slice(&1u64.to_be_bytes());
        k
    };
    let acc_value = borsh::to_vec(&account).unwrap();
    batch.put("accounts", acc_key.to_vec(), acc_value);

    // Account index
    batch.put(
        "account_index",
        addr.as_bytes().to_vec(),
        1u64.to_be_bytes().to_vec(),
    );

    // Bank hash
    let bank_hash = hash(b"bank_1");
    batch.put(
        "bank_hashes",
        1u64.to_be_bytes().to_vec(),
        bank_hash.as_bytes().to_vec(),
    );

    // Root marker
    batch.put("roots", 1u64.to_be_bytes().to_vec(), vec![]);

    assert_eq!(batch.len(), 4);
    assert!(!batch.is_empty());

    storage.write(&batch).unwrap();

    // Verify all writes landed
    let loaded = storage.get_account(&addr).unwrap().unwrap();
    assert_eq!(loaded.lamports, 1000);
    assert_eq!(storage.get_bank_hash(1).unwrap().unwrap(), bank_hash);
    assert!(storage.is_root(1).unwrap());
}

#[test]
fn batch_put_and_delete_in_same_batch() {
    let (storage, _dir) = temp_storage();

    // Pre-populate
    storage.set_root(1).unwrap();
    storage.set_root(2).unwrap();

    // Batch: delete root 1, keep root 2, add root 3
    let mut batch = StorageWriteBatch::new();
    batch.delete("roots", 1u64.to_be_bytes().to_vec());
    batch.put("roots", 3u64.to_be_bytes().to_vec(), vec![]);
    storage.write(&batch).unwrap();

    assert!(!storage.is_root(1).unwrap()); // deleted
    assert!(storage.is_root(2).unwrap()); // untouched
    assert!(storage.is_root(3).unwrap()); // added
}

#[test]
fn empty_batch_is_noop() {
    let (storage, _dir) = temp_storage();
    let batch = StorageWriteBatch::new();
    assert!(batch.is_empty());
    assert_eq!(batch.len(), 0);
    storage.write(&batch).unwrap(); // should not fail
}

#[test]
fn large_batch() {
    let (storage, _dir) = temp_storage();

    let mut batch = StorageWriteBatch::new();
    for slot in 0..1000u64 {
        batch.put("roots", slot.to_be_bytes().to_vec(), vec![]);
    }
    assert_eq!(batch.len(), 1000);

    storage.write(&batch).unwrap();

    for slot in 0..1000u64 {
        assert!(storage.is_root(slot).unwrap());
    }
    assert!(!storage.is_root(1000).unwrap());
}

#[test]
fn default_batch_is_empty() {
    let batch = StorageWriteBatch::default();
    assert!(batch.is_empty());
    assert_eq!(batch.len(), 0);
}
