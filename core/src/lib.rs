pub mod error;
pub mod native_token;
pub mod instruction;
pub mod message;
pub mod transaction;
pub mod account;
pub mod batch_transaction;
pub mod block;
pub mod epoch;
pub mod fee;
pub mod program;
pub mod commitment;

pub use error::CoreError;
pub use native_token::{LAMPORTS_PER_NUSA, lamports_to_nusa, nusa_to_lamports};
pub use instruction::{AccountMeta, CompiledInstruction, Instruction};
pub use message::Message;
pub use transaction::Transaction;
pub use account::Account;
pub use batch_transaction::{BatchEntry, SignedTransactionBatch};
pub use block::{Block, BlockHeader};
pub use epoch::{DEFAULT_SLOTS_PER_EPOCH, DEFAULT_SLOT_DURATION_MS, EpochSchedule};
pub use fee::{DEFAULT_LAMPORTS_PER_SIGNATURE, FeeCalculator};
pub use program::{
    SYSTEM_PROGRAM_ID, RENT_PROGRAM_ID, STAKE_PROGRAM_ID,
    VOTE_PROGRAM_ID, COMPUTE_BUDGET_PROGRAM_ID, SYSVAR_PROGRAM_ID,
    LOADER_PROGRAM_ID,
};
pub use commitment::CommitmentLevel;

/// Maximum account data size in bytes (10 MiB).
pub const MAX_ACCOUNT_DATA_SIZE: u64 =
    native_token::const_parse_u64(env!("NUSA_ACCOUNT_MAX_DATA_SIZE"));
