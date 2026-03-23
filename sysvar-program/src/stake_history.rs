use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::{Hash, hash};

use crate::Sysvar;

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct StakeHistoryEntry {
    pub effective: u64,
    pub activating: u64,
    pub deactivating: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct StakeHistory(pub Vec<(u64, StakeHistoryEntry)>);

impl StakeHistory {
    pub fn new(entries: Vec<(u64, StakeHistoryEntry)>) -> Self {
        Self(entries)
    }

    pub fn get(&self, epoch: u64) -> Option<&StakeHistoryEntry> {
        self.0
            .iter()
            .find(|(e, _)| *e == epoch)
            .map(|(_, entry)| entry)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Sysvar for StakeHistory {
    fn id() -> Hash {
        hash(b"sysvar_stake_history")
    }

    fn size_of() -> usize {
        // Variable size
        // Each entry: u64 (epoch) + 3 * u64 = 32 bytes
        // Max 512 entries: 512 * 32 + 4 (vec len) = 16388
        16388
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borsh_roundtrip() {
        let history = StakeHistory::new(vec![(
            1,
            StakeHistoryEntry {
                effective: 1000,
                activating: 500,
                deactivating: 200,
            },
        )]);
        let encoded = borsh::to_vec(&history).unwrap();
        let decoded: StakeHistory = borsh::from_slice(&encoded).unwrap();
        assert_eq!(history, decoded);
    }

    #[test]
    fn get_epoch() {
        let entry = StakeHistoryEntry {
            effective: 100,
            activating: 50,
            deactivating: 25,
        };
        let history = StakeHistory::new(vec![(5, entry.clone())]);
        assert_eq!(history.get(5), Some(&entry));
        assert_eq!(history.get(6), None);
    }
}
