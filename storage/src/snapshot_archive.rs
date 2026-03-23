use std::io::{Read, Write};
use std::path::Path;

use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_core::Account;
use nusantara_crypto::Hash;
use rocksdb::IteratorMode;

use crate::cf::{CF_ACCOUNT_INDEX, CF_BANK_HASHES, CF_DEFAULT, CF_ROOTS, CF_SNAPSHOTS};
use crate::error::StorageError;
use crate::keys::slot_key;
use crate::snapshot::SnapshotManifest;
use crate::storage::Storage;
use crate::write_batch::StorageWriteBatch;

/// Marker key written to `CF_DEFAULT` at the start of a snapshot restore.
/// Removed on successful completion. If present at boot, a previous restore
/// was interrupted and the partial state must be cleaned up.
pub const SNAPSHOT_RESTORE_MARKER: &[u8] = b"snapshot_restore_in_progress";

/// A snapshot archive containing all state needed to bootstrap a validator.
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct SnapshotArchive {
    pub manifest: SnapshotManifest,
    pub accounts: Vec<(Hash, Account)>,
}

/// Create a snapshot of the current state.
pub fn create_snapshot(
    storage: &Storage,
    slot: u64,
    bank_hash: Hash,
    timestamp: i64,
) -> Result<SnapshotArchive, StorageError> {
    // Collect all current accounts from the account index
    let cf_index = storage
        .db
        .cf_handle(CF_ACCOUNT_INDEX)
        .ok_or(StorageError::CfNotFound(CF_ACCOUNT_INDEX))?;

    let mut accounts = Vec::new();

    let iter = storage.db.iterator_cf(cf_index, IteratorMode::Start);
    for item in iter {
        let (key, _value) = item.map_err(StorageError::RocksDb)?;
        if key.len() != 64 {
            continue;
        }
        let address = Hash::new(
            key[..64]
                .try_into()
                .map_err(|_| StorageError::Corruption("invalid address".into()))?,
        );

        if let Some(account) = storage.get_account(&address)? {
            accounts.push((address, account));
        }
    }

    let manifest = SnapshotManifest {
        slot,
        bank_hash,
        account_count: accounts.len() as u64,
        timestamp,
    };

    // Store manifest in storage
    storage.put_snapshot(&manifest)?;

    Ok(SnapshotArchive { manifest, accounts })
}

/// Save a snapshot archive to a file (borsh-serialized).
///
/// Uses write-to-tmp + fsync + atomic rename to prevent corruption if the
/// process crashes mid-write. The `.tmp` sibling file is created in the same
/// directory so `rename()` is guaranteed to be atomic (same filesystem).
pub fn save_to_file(archive: &SnapshotArchive, path: &Path) -> Result<(), StorageError> {
    let bytes = borsh::to_vec(archive).map_err(|e| StorageError::Serialization(e.to_string()))?;

    // Write to a temporary file in the same directory, then atomically rename.
    let tmp_path = path.with_extension("bin.tmp");
    let mut file =
        std::fs::File::create(&tmp_path).map_err(|e| StorageError::Io(e.to_string()))?;
    file.write_all(&bytes)
        .map_err(|e| StorageError::Io(e.to_string()))?;
    file.sync_all()
        .map_err(|e| StorageError::Io(e.to_string()))?;
    std::fs::rename(&tmp_path, path).map_err(|e| StorageError::Io(e.to_string()))?;
    Ok(())
}

/// Maximum allowed snapshot file size (10 GiB).
///
/// Prevents out-of-memory when loading a malicious or corrupt file.
const MAX_SNAPSHOT_SIZE: u64 = 10 * 1024 * 1024 * 1024;

/// Load a snapshot archive from a file.
///
/// Rejects files larger than [`MAX_SNAPSHOT_SIZE`] to guard against OOM on
/// corrupt or malicious inputs.
pub fn load_from_file(path: &Path) -> Result<SnapshotArchive, StorageError> {
    let file = std::fs::File::open(path).map_err(|e| StorageError::Io(e.to_string()))?;
    let file_size = file
        .metadata()
        .map_err(|e| StorageError::Io(e.to_string()))?
        .len();
    if file_size > MAX_SNAPSHOT_SIZE {
        return Err(StorageError::Corruption(format!(
            "snapshot file too large: {file_size} bytes exceeds {MAX_SNAPSHOT_SIZE} byte limit"
        )));
    }

    let mut reader = std::io::BufReader::new(file);
    let mut bytes = Vec::with_capacity(file_size as usize);
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| StorageError::Io(e.to_string()))?;
    let archive = SnapshotArchive::try_from_slice(&bytes)
        .map_err(|e| StorageError::Deserialization(e.to_string()))?;
    Ok(archive)
}

/// Bootstrap storage from a snapshot archive.
///
/// Accumulates all account writes and the snapshot manifest into a single
/// atomic `StorageWriteBatch` to prevent partial state on crash.
pub fn bootstrap_from_snapshot(
    storage: &Storage,
    archive: &SnapshotArchive,
) -> Result<(), StorageError> {
    let slot = archive.manifest.slot;
    let mut batch = StorageWriteBatch::new();

    // Accumulate all account writes
    for (address, account) in &archive.accounts {
        storage.append_account_write(&mut batch, address, slot, account)?;
    }

    // Store the snapshot manifest
    let manifest_key = slot_key(slot);
    let manifest_value = borsh::to_vec(&archive.manifest)
        .map_err(|e| StorageError::Serialization(e.to_string()))?;
    batch.put(CF_SNAPSHOTS, manifest_key.to_vec(), manifest_value);

    storage.write(&batch)?;
    Ok(())
}

/// Restore state from a snapshot archive.
///
/// This performs a full state restore: writes all accounts, stores the manifest,
/// sets the snapshot slot as a finalized root, and records the bank hash.
/// After calling this, the validator can resume from the snapshot slot
/// without needing to replay from genesis.
///
/// All writes are accumulated into a single `StorageWriteBatch` and committed
/// atomically. A `SNAPSHOT_RESTORE_MARKER` is written before the batch and
/// removed as the last operation in the batch; if the process crashes mid-
/// restore, the marker's presence at next boot signals a partial restore
/// that must be cleaned up (see [`cleanup_partial_snapshot_restore`]).
pub fn restore_snapshot(storage: &Storage, archive: &SnapshotArchive) -> Result<(), StorageError> {
    let slot = archive.manifest.slot;

    // Write the in-progress marker BEFORE the atomic batch so that a crash
    // between marker-write and batch-commit is detectable.
    storage.put_cf(CF_DEFAULT, SNAPSHOT_RESTORE_MARKER, &slot.to_le_bytes())?;

    let mut batch = StorageWriteBatch::new();

    // 1. Accumulate all account writes into the batch
    for (address, account) in &archive.accounts {
        storage.append_account_write(&mut batch, address, slot, account)?;
    }

    // 2. Store the snapshot manifest
    let manifest_key = slot_key(slot);
    let manifest_value = borsh::to_vec(&archive.manifest)
        .map_err(|e| StorageError::Serialization(e.to_string()))?;
    batch.put(CF_SNAPSHOTS, manifest_key.to_vec(), manifest_value);

    // 3. Mark the snapshot slot as a finalized root
    batch.put(CF_ROOTS, slot_key(slot).to_vec(), Vec::new());

    // 4. Store the bank hash so the validator can reconstruct parent state
    batch.put(
        CF_BANK_HASHES,
        slot_key(slot).to_vec(),
        archive.manifest.bank_hash.as_bytes().to_vec(),
    );

    // 5. Remove the in-progress marker (completes the restore)
    batch.delete(CF_DEFAULT, SNAPSHOT_RESTORE_MARKER.to_vec());

    // Commit everything atomically
    storage.write(&batch)?;
    Ok(())
}

/// Check for and clean up a partial snapshot restore from a previous crash.
///
/// If the `SNAPSHOT_RESTORE_MARKER` key is present in `CF_DEFAULT`, a prior
/// `restore_snapshot` call was interrupted. This function deletes the marker
/// so the caller can re-attempt snapshot restore or fall through to genesis.
///
/// Returns `Ok(true)` if a partial restore was detected and cleaned up,
/// `Ok(false)` if no marker was found.
pub fn cleanup_partial_snapshot_restore(storage: &Storage) -> Result<bool, StorageError> {
    if storage.get_cf(CF_DEFAULT, SNAPSHOT_RESTORE_MARKER)?.is_some() {
        // The atomic batch never committed — the marker is the only evidence.
        // Delete it so the validator can retry or proceed to genesis.
        storage.delete_cf(CF_DEFAULT, SNAPSHOT_RESTORE_MARKER)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Find the latest snapshot file in the given directory.
///
/// Scans for files matching the pattern `snapshot-{slot}.bin` and returns
/// the path to the one with the highest slot number.
pub fn find_latest_snapshot_file(dir: &Path) -> Option<std::path::PathBuf> {
    let read_dir = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(u64, std::path::PathBuf)> = None;

    for entry in read_dir.flatten() {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && let Some(slot_str) = name
                .strip_prefix("snapshot-")
                .and_then(|s| s.strip_suffix(".bin"))
            && let Ok(slot) = slot_str.parse::<u64>()
        {
            match &best {
                Some((best_slot, _)) if slot > *best_slot => {
                    best = Some((slot, path));
                }
                None => {
                    best = Some((slot, path));
                }
                _ => {}
            }
        }
    }

    best.map(|(_, path)| path)
}
