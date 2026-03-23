use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::Hash;

pub mod clock;
pub mod epoch_schedule;
pub mod recent_blockhashes;
pub mod rent;
pub mod slot_hashes;
pub mod stake_history;

pub use clock::Clock;
pub use epoch_schedule::EpochScheduleSysvar;
pub use recent_blockhashes::RecentBlockhashes;
pub use rent::RentSysvar;
pub use slot_hashes::SlotHashes;
pub use stake_history::{StakeHistory, StakeHistoryEntry};

pub trait Sysvar: BorshSerialize + BorshDeserialize + Sized {
    fn id() -> Hash;
    fn size_of() -> usize;
}
