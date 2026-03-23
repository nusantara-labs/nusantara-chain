use std::time::{SystemTime, UNIX_EPOCH};

use nusantara_consensus::bank::ConsensusBank;
use nusantara_core::EpochSchedule;
use nusantara_crypto::{Hash, MerkleTree};
use nusantara_core::Transaction;
use nusantara_rent_program::Rent;
use nusantara_runtime::SysvarCache;
use nusantara_storage::Storage;
use nusantara_sysvar_program::RecentBlockhashes;

use crate::constants::RECENT_BLOCKHASHES_COUNT;

/// Current Unix timestamp in seconds (i64).
pub(crate) fn unix_timestamp_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_secs() as i64
}

/// Current Unix timestamp in milliseconds (u64).
#[allow(dead_code)]
pub(crate) fn unix_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_millis() as u64
}

/// Build a SysvarCache from the current bank state.
pub(crate) fn build_sysvar_cache(
    bank: &ConsensusBank,
    rent: &Rent,
    epoch_schedule: &EpochSchedule,
) -> SysvarCache {
    let clock = bank.clock();
    let slot_hashes = bank.slot_hashes();
    let stake_history = bank.stake_history();
    let recent_blockhashes = RecentBlockhashes::new(
        slot_hashes
            .0
            .iter()
            .take(RECENT_BLOCKHASHES_COUNT)
            .map(|(_, h)| *h)
            .collect(),
    );
    SysvarCache::new(
        clock,
        rent.clone(),
        epoch_schedule.clone(),
        slot_hashes,
        stake_history,
        recent_blockhashes,
    )
}

/// Build slot_hashes by merging fork tree ancestry (post-root, live) with
/// historical hashes from CF_SLOT_HASHES (pre-root, finalized).
///
/// The fork tree only retains nodes from root to tips (~30 after pruning).
/// The leader built its slot_hashes incrementally via `record_slot_hash()`,
/// accumulating up to 512 entries. To match the leader's view, we backfill
/// from storage for slots below the fork tree root.
pub(crate) fn build_merged_slot_hashes(
    fork_ancestry: &[(u64, Hash)],
    storage: &Storage,
    max_entries: usize,
) -> Vec<(u64, Hash)> {
    let mut merged = fork_ancestry.to_vec();

    if merged.len() >= max_entries {
        merged.truncate(max_entries);
        return merged;
    }

    // Find the lowest slot in fork ancestry to know where to start backfill
    let backfill_below = merged.iter().map(|(s, _)| *s).min().unwrap_or(0);
    if backfill_below == 0 {
        return merged;
    }

    // Backfill from storage (finalized slots below fork tree root)
    let need = max_entries - merged.len();
    if let Ok(historical) =
        storage.get_recent_slot_hashes_below(backfill_below.saturating_sub(1), need)
    {
        merged.extend(historical);
    }

    merged
}

/// Compute the Merkle root of a list of transactions.
pub(crate) fn compute_merkle_root(transactions: &[Transaction]) -> Hash {
    if transactions.is_empty() {
        Hash::zero()
    } else {
        let tx_hashes: Vec<Hash> = transactions.iter().map(|tx| tx.hash()).collect();
        MerkleTree::new(&tx_hashes).root()
    }
}
