use nusantara_storage::StorageError;

#[derive(Debug, thiserror::Error)]
pub enum ConsensusError {
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    // PoH errors
    #[error("PoH verification failed at entry {index}")]
    PohVerificationFailed { index: usize },
    #[error("PoH hash chain broken: expected {expected}, got {got}")]
    PohChainBroken { expected: String, got: String },

    // Tower errors
    #[error("lockout violation: vote at slot {vote_slot} violates lockout at slot {locked_slot}")]
    LockoutViolation { vote_slot: u64, locked_slot: u64 },
    #[error("vote too old: slot {vote_slot} is at or before root slot {root_slot}")]
    VoteTooOld { vote_slot: u64, root_slot: u64 },
    #[error("insufficient stake for switch threshold: have {have_pct}%, need {need_pct}%")]
    InsufficientStakeForThreshold { have_pct: u64, need_pct: u64 },

    // Fork choice errors
    #[error("slot {0} already exists in fork tree")]
    SlotAlreadyProcessed(u64),
    #[error("parent slot {parent} not found for slot {child}")]
    ParentNotFound { parent: u64, child: u64 },
    #[error("fork tree depth exceeded: {depth} > {max}")]
    MaxDepthExceeded { depth: u64, max: u64 },

    // Leader schedule errors
    #[error("no validators with stake for epoch {0}")]
    NoValidatorsWithStake(u64),
    #[error("wrong leader for slot {slot}: expected {expected}, got {got}")]
    WrongLeader {
        slot: u64,
        expected: String,
        got: String,
    },

    // Bank errors
    #[error("vote account not found: {0}")]
    VoteAccountNotFound(String),
    #[error("epoch mismatch: expected {expected}, got {got}")]
    EpochMismatch { expected: u64, got: u64 },

    // Replay errors
    #[error("block for slot {0} failed replay")]
    ReplayFailed(u64),

    // Commitment errors
    #[error("slot {0} not tracked in commitment")]
    SlotNotTracked(u64),

    // Rewards errors
    #[error("no epoch credits for reward calculation")]
    NoEpochCredits,
    #[error("zero total stake for reward distribution")]
    ZeroTotalStake,

    // GPU errors
    #[error("GPU error: {0}")]
    Gpu(String),
    #[error("GPU not available")]
    GpuNotAvailable,
}
