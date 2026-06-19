use std::collections::HashMap;
use std::sync::Arc;

use borsh::{BorshDeserialize, BorshSerialize};
use dashmap::DashMap;
use nusantara_core::epoch::EpochSchedule;
use nusantara_crypto::Hash;
use nusantara_stake_program::Delegation;
use nusantara_storage::Storage;
use nusantara_sysvar_program::{Clock, SlotHashes, StakeHistory};
use nusantara_vote_program::VoteState;
use parking_lot::{Mutex, RwLock};

use crate::state_tree::StateTree;

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct FrozenBankState {
    pub slot: u64,
    pub parent_slot: u64,
    pub block_hash: Hash,
    pub bank_hash: Hash,
    pub epoch: u64,
    pub transaction_count: u64,
}

/// Holds epoch-level stake data behind a single lock to guarantee atomic reads.
/// Previously `epoch_stakes`, `total_active_stake`, and `cached_stake_vec` were
/// stored in separate `RwLock`s, allowing readers to observe partially-updated
/// state during `recalculate_epoch_stakes`.
pub(crate) struct EpochStakeState {
    pub epoch_stakes: HashMap<Hash, u64>,
    pub total_active_stake: u64,
    pub cached_stake_vec: Arc<Vec<(Hash, u64)>>,
}

impl Default for EpochStakeState {
    fn default() -> Self {
        Self {
            epoch_stakes: HashMap::new(),
            total_active_stake: 0,
            cached_stake_vec: Arc::new(Vec::new()),
        }
    }
}

pub struct ConsensusBank {
    pub(crate) storage: Arc<Storage>,
    pub(crate) epoch_schedule: EpochSchedule,
    pub(crate) vote_accounts: DashMap<Hash, VoteState>,
    pub(crate) stake_delegations: DashMap<Hash, Delegation>,
    /// Combined epoch stake state behind a single lock for atomic reads/writes.
    pub(crate) epoch_stake_state: RwLock<EpochStakeState>,
    pub(crate) total_supply: RwLock<u64>,
    /// Arc-wrapped so readers can clone a cheap Arc<Clock> instead of cloning
    /// the full struct. Writers use Arc::make_mut which only copies when multiple
    /// readers hold a concurrent Arc (copy-on-write; single-writer steady state is free).
    pub(crate) clock: RwLock<Arc<Clock>>,
    pub(crate) slot_hashes: RwLock<Arc<SlotHashes>>,
    pub(crate) stake_history: RwLock<Arc<StakeHistory>>,
    pub(crate) current_slot: RwLock<u64>,
    /// Validator identity -> total slashed lamports. Reduces effective stake
    /// without modifying the Delegation structs (avoids serialization breakage).
    ///
    /// B30 lock order: slash_registry (DashMap, shard-level) is never held
    /// concurrently with epoch_stake_state. Callers must not hold an
    /// epoch_stake_state write-guard while calling get_slashed_amount().
    pub(crate) slash_registry: DashMap<Hash, u64>,
    /// Incremental Merkle tree over all account state.
    /// Protected by a `Mutex` (not held across `.await` points).
    pub(crate) state_tree: Mutex<StateTree>,
}

impl ConsensusBank {
    pub fn new(storage: Arc<Storage>, epoch_schedule: EpochSchedule) -> Self {
        Self {
            storage,
            epoch_schedule,
            vote_accounts: DashMap::new(),
            stake_delegations: DashMap::new(),
            epoch_stake_state: RwLock::new(EpochStakeState::default()),
            total_supply: RwLock::new(0),
            clock: RwLock::new(Arc::new(Clock::default())),
            slot_hashes: RwLock::new(Arc::new(SlotHashes::default())),
            stake_history: RwLock::new(Arc::new(StakeHistory::default())),
            current_slot: RwLock::new(0),
            slash_registry: DashMap::new(),
            state_tree: Mutex::new(StateTree::new()),
        }
    }

    pub fn storage(&self) -> &Arc<Storage> {
        &self.storage
    }

    pub fn epoch_schedule(&self) -> &EpochSchedule {
        &self.epoch_schedule
    }

    pub fn current_slot(&self) -> u64 {
        *self.current_slot.read()
    }

    pub fn current_epoch(&self) -> u64 {
        self.epoch_schedule.get_epoch(self.current_slot())
    }
}

#[cfg(test)]
mod tests {
    use crate::test_utils::test_helpers::temp_bank;

    #[test]
    fn new_bank() {
        let (bank, _storage, _dir) = temp_bank();
        assert_eq!(bank.current_slot(), 0);
        assert_eq!(bank.current_epoch(), 0);
        assert_eq!(bank.total_active_stake(), 0);
    }

    #[test]
    fn epoch_boundary_detection() {
        let (bank, _storage, _dir) = temp_bank();
        bank.advance_slot(99, 1000);
        assert_eq!(bank.current_epoch(), 0);
        bank.advance_slot(100, 1001);
        assert_eq!(bank.current_epoch(), 1);
    }
}
