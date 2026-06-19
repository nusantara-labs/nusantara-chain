use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use nusantara_core::block::Block;
use nusantara_crypto::Hash;
use tokio::sync::watch;
use tracing::instrument;

use crate::bank::ConsensusBank;
use crate::commitment::CommitmentTracker;
use crate::fork_choice::ForkTree;
use crate::gpu::GpuPohVerifier;
use crate::leader_schedule::{LeaderSchedule, LeaderScheduleGenerator};
use crate::poh::PohEntry;
use crate::tower::Tower;

#[derive(Clone, Debug)]
pub struct ReplayResult {
    pub slot: u64,
    pub block_hash: Hash,
    pub bank_hash: Hash,
    pub parent_slot: u64,
    pub transaction_count: u64,
    pub vote_count: u64,
    pub new_root: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct ForkSwitchPlan {
    pub common_ancestor: u64,
    pub rollback_from: u64,
    pub replay_slots: Vec<u64>,
}

pub struct ReplayStage {
    /// Node identity (public key hash). Retained for leader verification
    /// and future multi-validator scenarios.
    #[allow(dead_code)]
    pub(crate) identity: Hash,
    pub(crate) authorized_voter: Hash,
    pub(crate) bank: Arc<ConsensusBank>,
    pub(crate) tower: Tower,
    pub(crate) fork_tree: ForkTree,
    pub(crate) commitment_tracker: CommitmentTracker,
    pub(crate) leader_schedule_cache: HashMap<u64, LeaderSchedule>,
    pub(crate) leader_schedule_generator: LeaderScheduleGenerator,
    pub(crate) gpu_verifier: Option<GpuPohVerifier>,
    pub(crate) current_tip: u64,
    /// Gossip votes for slots not yet in the fork tree. Drained when the
    /// slot is added via `replay_block`. BTreeMap keeps slots sorted so oldest
    /// entries can be evicted via pop_first() in O(log N) (B14).
    /// Each entry maps a slot to a list of `(voter, block_hash, stake)` tuples.
    pub(crate) pending_votes: BTreeMap<u64, Vec<(Hash, Hash, u64)>>,
    /// PoH hash at the end of the last successfully replayed slot.
    /// Initialized to `Hash::zero()` (valid for the genesis slot).
    /// Passed as `parent_poh` to `replay_block` so PoH continuity is
    /// verified across slot boundaries even when `poh_entries` is non-empty.
    pub(crate) last_poh_hash: Hash,
}

impl ReplayStage {
    pub fn new(
        identity: Hash,
        bank: Arc<ConsensusBank>,
        tower: Tower,
        fork_tree: ForkTree,
        commitment_tracker: CommitmentTracker,
        gpu_verifier: Option<GpuPohVerifier>,
    ) -> Self {
        let epoch_schedule = bank.epoch_schedule().clone();
        let initial_tip = fork_tree.root_slot();
        let authorized_voter = tower.vote_state().authorized_voter;
        Self {
            identity,
            authorized_voter,
            bank,
            tower,
            fork_tree,
            commitment_tracker,
            leader_schedule_cache: HashMap::new(),
            leader_schedule_generator: LeaderScheduleGenerator::new(epoch_schedule),
            gpu_verifier,
            current_tip: initial_tip,
            pending_votes: BTreeMap::new(),
            last_poh_hash: Hash::zero(),
        }
    }

    pub fn tower(&self) -> &Tower {
        &self.tower
    }

    pub fn fork_tree(&self) -> &ForkTree {
        &self.fork_tree
    }

    pub fn commitment_tracker(&self) -> &CommitmentTracker {
        &self.commitment_tracker
    }

    pub fn bank(&self) -> &Arc<ConsensusBank> {
        &self.bank
    }

    pub fn current_tip(&self) -> u64 {
        self.current_tip
    }

    /// Main async replay loop.
    #[instrument(skip(self, block_receiver, shutdown), level = "info")]
    pub async fn run(
        &mut self,
        mut block_receiver: tokio::sync::mpsc::Receiver<(Block, Vec<PohEntry>)>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        tracing::info!("ReplayStage started");

        loop {
            // Bias toward processing blocks over shutdown
            tokio::select! {
                biased;
                Some((block, poh_entries)) = block_receiver.recv() => {
                    let slot = block.header.slot;
                    // Pass the cached last PoH hash so replay_block can verify
                    // PoH continuity from the parent slot's terminal hash (F12).
                    let parent_poh = self.last_poh_hash;
                    match self.replay_block(&block, &poh_entries, &parent_poh) {
                        Ok(result) => {
                            // Update last_poh_hash to the terminal PoH hash of this slot.
                            // If poh_entries is non-empty use the last entry's hash;
                            // otherwise keep the parent (no PoH advancement on empty slots).
                            if let Some(last_entry) = poh_entries.last() {
                                self.last_poh_hash = last_entry.hash;
                            }
                            // In standalone run() mode, always advance root
                            if let Some(&root) = result.new_root.as_ref()
                                && let Err(e) = self.advance_root(root)
                            {
                                tracing::warn!(?e, root, "root advancement failed");
                            }
                            tracing::info!(
                                slot = result.slot,
                                votes = result.vote_count,
                                root = ?result.new_root,
                                "Block replayed successfully"
                            );
                        }
                        Err(e) => {
                            tracing::error!(slot, ?e, "Block replay failed");
                        }
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("ReplayStage shutting down");
                        break;
                    }
                }
            }
        }
    }
}
