use std::collections::{BTreeMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use nusantara_consensus::bank::ConsensusBank;
use nusantara_consensus::leader_schedule::LeaderScheduleGenerator;
use nusantara_consensus::replay_stage::ReplayStage;
use nusantara_core::FeeCalculator;
use nusantara_core::block::Block;
use nusantara_core::epoch::EpochSchedule;
use nusantara_crypto::{Hash, Keypair};
use nusantara_gossip::ClusterInfo;
use nusantara_mempool::Mempool;
use nusantara_rpc::PubsubEvent;
use nusantara_runtime::ProgramCache;
use nusantara_storage::Storage;
use nusantara_turbine::ShredCollector;
use tokio::sync::broadcast;

use crate::block_producer::BlockProducer;
use crate::constants::SharedLeaderCache;
use crate::error::ValidatorError;
use crate::slot_clock::SlotClock;

pub struct ValidatorNode {
    // Identity
    pub(crate) keypair: Arc<Keypair>,
    pub(crate) identity: Hash,

    // Storage & Consensus
    pub(crate) storage: Arc<Storage>,
    pub(crate) bank: Arc<ConsensusBank>,
    pub(crate) block_producer: BlockProducer,

    // Transactions
    pub(crate) mempool: Arc<Mempool>,

    // Timing
    pub(crate) slot_clock: SlotClock,
    pub(crate) current_slot: u64,

    // Networking
    pub(crate) cluster_info: Arc<ClusterInfo>,

    // Consensus engine
    pub(crate) replay_stage: ReplayStage,

    // Leader schedule
    pub(crate) leader_cache: SharedLeaderCache,
    pub(crate) leader_schedule_generator: LeaderScheduleGenerator,
    pub(crate) epoch_schedule: EpochSchedule,
    pub(crate) genesis_hash: Hash,

    // Vote account
    pub(crate) my_vote_account: Option<Hash>,

    // Network addresses
    pub(crate) gossip_addr: SocketAddr,
    pub(crate) turbine_addr: SocketAddr,
    pub(crate) repair_addr: SocketAddr,
    pub(crate) tpu_addr: SocketAddr,
    #[allow(dead_code)]
    pub(crate) tpu_forward_addr: SocketAddr,

    // Skip tracking (F1/F5)
    pub(crate) consecutive_skips: Arc<AtomicU64>,
    pub(crate) total_skips: u64,

    // Replay progress counter shared with background services (repair, retransmit).
    // Updated after every successful block replay; used for eviction decisions
    // instead of wall-clock current_slot to prevent catch-up death spirals.
    pub(crate) replay_tip_shared: Arc<AtomicU64>,

    // Gossip vote cursor (F4)
    pub(crate) gossip_vote_cursor: u64,

    // Slash detection (F3)
    pub(crate) slash_detector: nusantara_consensus::SlashDetector,

    // Fee/rent for block replay (F2)
    pub(crate) fee_calculator: FeeCalculator,
    pub(crate) rent: nusantara_rent_program::Rent,

    // WASM program cache
    pub(crate) program_cache: Arc<ProgramCache>,

    // WebSocket pubsub broadcast channel
    pub(crate) pubsub_tx: broadcast::Sender<PubsubEvent>,

    // Orphan block buffer (blocks whose parents haven't arrived yet)
    pub(crate) orphan_blocks: BTreeMap<u64, Block>,

    // Shared shred collector for requesting repair
    pub(crate) shred_collector: Arc<ShredCollector>,

    // Snapshot output directory
    pub(crate) snapshot_dir: PathBuf,

    // Track fork switch targets that have failed to prevent infinite retry.
    // Cleared when root advances (fork landscape changes).
    pub(crate) failed_fork_targets: HashSet<u64>,

    // Last slot we submitted a vote for (used to batch unvoted slots)
    pub(crate) last_voted_slot: u64,

    // Parent slot used in the last leader block production.
    // When `Some(parent)` and the new parent == parent + 1 (linear extension),
    // we skip the expensive account index rewind (no fork switch occurred).
    pub(crate) last_produced_parent: Option<u64>,

    // Dedup: skip fork switch if the target is the same as last attempt.
    // Reset to None when root advances (fork landscape genuinely changes).
    pub(crate) last_fork_switch_target: Option<u64>,

    // Maximum transactions to drain from mempool per slot
    pub(crate) max_txs_per_slot: usize,
}

impl ValidatorNode {
    /// Flush storage to disk (memtables + WAL -> SST files).
    pub fn flush_storage(&self) -> Result<(), ValidatorError> {
        self.storage.flush_all()?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn mempool(&self) -> Arc<Mempool> {
        Arc::clone(&self.mempool)
    }
}
