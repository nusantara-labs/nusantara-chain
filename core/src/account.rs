use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::Hash;

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Account {
    pub lamports: u64,
    pub data: Vec<u8>,
    pub owner: Hash,
    pub executable: bool,
    pub rent_epoch: u64,
}

impl Account {
    pub fn new(lamports: u64, owner: Hash) -> Self {
        Self {
            lamports,
            data: Vec::new(),
            owner,
            executable: false,
            rent_epoch: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lamports == 0 && self.data.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    #[test]
    fn new_account() {
        let owner = hash(b"system");
        let acc = Account::new(1000, owner);
        assert_eq!(acc.lamports, 1000);
        assert!(acc.data.is_empty());
        assert!(!acc.executable);
        assert_eq!(acc.rent_epoch, 0);
    }

    #[test]
    fn is_empty() {
        let owner = hash(b"system");
        let empty = Account::new(0, owner);
        assert!(empty.is_empty());

        let funded = Account::new(100, owner);
        assert!(!funded.is_empty());

        let mut with_data = Account::new(0, owner);
        with_data.data = vec![1, 2, 3];
        assert!(!with_data.is_empty());
    }

    #[test]
    fn borsh_roundtrip() {
        let owner = hash(b"owner");
        let mut acc = Account::new(42, owner);
        acc.data = vec![10, 20, 30];
        acc.executable = true;
        acc.rent_epoch = 5;

        let encoded = borsh::to_vec(&acc).unwrap();
        let decoded: Account = borsh::from_slice(&encoded).unwrap();
        assert_eq!(acc, decoded);
    }
}
