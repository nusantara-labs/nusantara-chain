use borsh::{BorshDeserialize, BorshSerialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum CommitmentLevel {
    Processed,
    Confirmed,
    Finalized,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borsh_roundtrip() {
        for level in [
            CommitmentLevel::Processed,
            CommitmentLevel::Confirmed,
            CommitmentLevel::Finalized,
        ] {
            let encoded = borsh::to_vec(&level).unwrap();
            let decoded: CommitmentLevel = borsh::from_slice(&encoded).unwrap();
            assert_eq!(level, decoded);
        }
    }
}
