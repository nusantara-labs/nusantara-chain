use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_core::epoch::EpochSchedule;
use nusantara_crypto::{Hash, hash};

use crate::Sysvar;

#[derive(Clone, Debug, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct EpochScheduleSysvar(pub EpochSchedule);

impl Sysvar for EpochScheduleSysvar {
    fn id() -> Hash {
        hash(b"sysvar_epoch_schedule")
    }

    fn size_of() -> usize {
        // EpochSchedule: 3 * u64 + bool + 2 * u64 = 41 bytes
        41
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borsh_roundtrip() {
        let sysvar = EpochScheduleSysvar::default();
        let encoded = borsh::to_vec(&sysvar).unwrap();
        let decoded: EpochScheduleSysvar = borsh::from_slice(&encoded).unwrap();
        assert_eq!(sysvar, decoded);
    }
}
