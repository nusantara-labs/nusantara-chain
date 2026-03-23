use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::{Hash, hash};

use crate::Sysvar;

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Clock {
    pub slot: u64,
    pub epoch_start_timestamp: i64,
    pub epoch: u64,
    pub leader_schedule_epoch: u64,
    pub unix_timestamp: i64,
}

impl Default for Clock {
    fn default() -> Self {
        Self {
            slot: 0,
            epoch_start_timestamp: 0,
            epoch: 0,
            leader_schedule_epoch: 1,
            unix_timestamp: 0,
        }
    }
}

impl Sysvar for Clock {
    fn id() -> Hash {
        hash(b"sysvar_clock")
    }

    fn size_of() -> usize {
        // 5 * u64/i64 = 40 bytes
        40
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borsh_roundtrip() {
        let clock = Clock {
            slot: 42,
            epoch_start_timestamp: 1000,
            epoch: 1,
            leader_schedule_epoch: 2,
            unix_timestamp: 1234,
        };
        let encoded = borsh::to_vec(&clock).unwrap();
        let decoded: Clock = borsh::from_slice(&encoded).unwrap();
        assert_eq!(clock, decoded);
    }

    #[test]
    fn id_is_deterministic() {
        assert_eq!(Clock::id(), Clock::id());
    }
}
