//! Shared test utilities for the runtime crate.
//!
//! Provides common helpers used across unit tests and integration tests
//! to avoid duplicating `test_sysvars()`, `test_storage()`, etc.
//!
//! This module is gated behind `#[cfg(any(test, feature = "test-utils"))]`.

use nusantara_core::EpochSchedule;
use nusantara_crypto::Hash;
use nusantara_rent_program::Rent;
use nusantara_storage::Storage;
use nusantara_sysvar_program::{Clock, RecentBlockhashes, SlotHashes, StakeHistory};
use nusantara_vm::ProgramCache;

use crate::sysvar_cache::SysvarCache;

/// Create a default `SysvarCache` suitable for most tests.
///
/// Includes `Hash::zero()` in recent blockhashes so tests using the default
/// blockhash don't need to construct a real one.
pub fn test_sysvars() -> SysvarCache {
    SysvarCache::new(
        Clock::default(),
        Rent::default(),
        EpochSchedule::default(),
        SlotHashes::default(),
        StakeHistory::default(),
        RecentBlockhashes::new(vec![Hash::zero()]),
    )
}

/// Create a `SysvarCache` with a custom clock (slot, epoch, unix_timestamp).
///
/// Useful for stake/vote tests that depend on epoch timing.
pub fn test_sysvars_with_clock(slot: u64, epoch: u64, unix_timestamp: i64) -> SysvarCache {
    SysvarCache::new(
        Clock {
            slot,
            epoch,
            unix_timestamp,
            ..Clock::default()
        },
        Rent::default(),
        EpochSchedule::default(),
        SlotHashes::default(),
        StakeHistory::default(),
        RecentBlockhashes::new(vec![Hash::zero()]),
    )
}

/// Create a temporary `Storage` + `TempDir` pair for tests.
///
/// The `TempDir` must be kept alive for the duration of the test.
pub fn test_storage() -> (Storage, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();
    (storage, dir)
}

/// Create a default `ProgramCache` with capacity 16.
pub fn test_cache() -> ProgramCache {
    ProgramCache::new(16)
}

/// Create a signed transfer transaction.
pub fn transfer_tx(
    from_kp: &nusantara_crypto::Keypair,
    to: nusantara_crypto::Hash,
    amount: u64,
) -> nusantara_core::Transaction {
    let from = from_kp.address();
    let ix = nusantara_system_program::transfer(&from, &to, amount);
    let msg = nusantara_core::Message::new(&[ix], &from).unwrap();
    let mut tx = nusantara_core::Transaction::new(msg);
    tx.sign(&[from_kp]);
    tx
}
