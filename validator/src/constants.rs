use std::collections::HashMap;
use std::sync::Arc;

use nusantara_consensus::leader_schedule::LeaderSchedule;

/// PublicKey (1952) + SecretKey (4032) = 5984 raw bytes.
pub(crate) const KEYPAIR_SIZE: usize = 1952 + 4032;

/// Maximum age (in slots) for orphan blocks and fork branches before they are
/// considered stale. Orphans older than this are evicted and no longer block
/// root advancement. 32 slots = 12.8s at 400ms/slot.
pub(crate) const ORPHAN_HORIZON: u64 = 32;

/// Safety valve for root advancement: if the gap between proposed and current
/// root exceeds this, force-advance bypassing orphan/fork gates.
pub(crate) const MAX_ROOT_GAP: u64 = 64;

/// Purge old slash detector entries every N slots.
pub(crate) const SLASH_PURGE_INTERVAL: u64 = 100;

/// Depth of slash detector history to retain (in slots).
pub(crate) const SLASH_PURGE_DEPTH: u64 = 1000;

/// Number of partitions for rent collection.
pub(crate) const RENT_PARTITIONS: u64 = 4096;

/// Prune old ledger entries every N slots.
pub(crate) const LEDGER_PRUNE_INTERVAL: u64 = 100;

/// Report gossip peer count every N slots.
pub(crate) const GOSSIP_REPORT_INTERVAL: u64 = 10;

/// Number of recent blockhashes to keep in the SysvarCache.
pub(crate) const RECENT_BLOCKHASHES_COUNT: usize = 300;

/// Timeout (ms) for ClusterInfo startup before considering peers lost.
pub(crate) const CLUSTER_INFO_TIMEOUT_MS: u64 = 60_000;

/// Maximum number of buffered orphan blocks before eviction.
pub(crate) const MAX_ORPHAN_BUFFER_SIZE: usize = 256;

/// Maximum number of slots in a single vote batch.
pub(crate) const MAX_VOTE_BATCH: u64 = 32;

/// Maximum entries in the WASM ProgramCache.
pub(crate) const PROGRAM_CACHE_SIZE: usize = 256;

/// Milliseconds in a calendar year (365 days).
pub(crate) const MS_PER_YEAR: u64 = 31_536_000_000;

/// Shared leader schedule cache type.
pub(crate) type SharedLeaderCache = Arc<parking_lot::RwLock<HashMap<u64, LeaderSchedule>>>;
