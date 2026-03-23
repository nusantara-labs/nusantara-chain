use nusantara_consensus::ConsensusError;
use nusantara_genesis::GenesisError;
use nusantara_gossip::GossipError;
use nusantara_runtime::RuntimeError;
use nusantara_storage::StorageError;
use nusantara_tpu_forward::TpuError;
use nusantara_turbine::TurbineError;

#[derive(Debug, thiserror::Error)]
pub enum ValidatorError {
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("consensus error: {0}")]
    Consensus(#[from] ConsensusError),

    #[error("runtime error: {0}")]
    Runtime(#[from] RuntimeError),

    #[error("genesis error: {0}")]
    Genesis(#[from] GenesisError),

    #[error("gossip error: {0}")]
    Gossip(#[from] GossipError),

    #[error("turbine error: {0}")]
    Turbine(#[from] TurbineError),

    #[error("TPU error: {0}")]
    Tpu(#[from] TpuError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("no genesis found in storage")]
    NoGenesis,

    #[error("keypair error: {0}")]
    Keypair(String),

    #[error("network initialization failed: {0}")]
    NetworkInit(String),

    #[error("validator shutdown")]
    Shutdown,

    #[error("bank hash mismatch at slot {slot}")]
    BankHashMismatch { slot: u64 },

    #[error("merkle root mismatch at slot {slot}")]
    MerkleRootMismatch { slot: u64 },

    #[error("block hash mismatch at slot {slot}")]
    BlockHashMismatch { slot: u64 },

    #[error("missing parent block: slot {slot} needs parent {parent_slot}")]
    MissingParentBlock { slot: u64, parent_slot: u64 },
}
