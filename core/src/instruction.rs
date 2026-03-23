use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::Hash;

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CompiledInstruction {
    pub program_id_index: u8,
    pub accounts: Vec<u8>,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Instruction {
    pub program_id: Hash,
    pub accounts: Vec<AccountMeta>,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct AccountMeta {
    pub pubkey: Hash,
    pub is_signer: bool,
    pub is_writable: bool,
}

impl AccountMeta {
    pub fn new(pubkey: Hash, is_signer: bool) -> Self {
        Self {
            pubkey,
            is_signer,
            is_writable: true,
        }
    }

    pub fn new_readonly(pubkey: Hash, is_signer: bool) -> Self {
        Self {
            pubkey,
            is_signer,
            is_writable: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    #[test]
    fn compiled_instruction_borsh_roundtrip() {
        let ix = CompiledInstruction {
            program_id_index: 2,
            accounts: vec![0, 1],
            data: vec![1, 2, 3, 4],
        };
        let encoded = borsh::to_vec(&ix).unwrap();
        let decoded: CompiledInstruction = borsh::from_slice(&encoded).unwrap();
        assert_eq!(ix, decoded);
    }

    #[test]
    fn account_meta_constructors() {
        let key = hash(b"test");
        let writable = AccountMeta::new(key, true);
        assert!(writable.is_signer);
        assert!(writable.is_writable);

        let readonly = AccountMeta::new_readonly(key, false);
        assert!(!readonly.is_signer);
        assert!(!readonly.is_writable);
    }
}
