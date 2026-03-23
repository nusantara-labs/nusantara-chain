use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_core::instruction::{AccountMeta, Instruction};
use nusantara_core::program::SYSTEM_PROGRAM_ID;
use nusantara_crypto::Hash;

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum SystemInstruction {
    CreateAccount {
        lamports: u64,
        space: u64,
        owner: Hash,
    },
    Transfer {
        lamports: u64,
    },
    Assign {
        owner: Hash,
    },
    Allocate {
        space: u64,
    },
    CreateAccountWithSeed {
        base: Hash,
        seed: String,
        lamports: u64,
        space: u64,
        owner: Hash,
    },
    AdvanceNonceAccount,
    WithdrawNonceAccount(u64),
    InitializeNonceAccount(Hash),
    AuthorizeNonceAccount(Hash),
}

pub fn create_account(
    from: &Hash,
    to: &Hash,
    lamports: u64,
    space: u64,
    owner: &Hash,
) -> Instruction {
    let data = borsh::to_vec(&SystemInstruction::CreateAccount {
        lamports,
        space,
        owner: *owner,
    })
    .expect("serialization cannot fail");

    Instruction {
        program_id: *SYSTEM_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*from, true),
            AccountMeta::new(*to, true),
        ],
        data,
    }
}

pub fn transfer(from: &Hash, to: &Hash, lamports: u64) -> Instruction {
    let data =
        borsh::to_vec(&SystemInstruction::Transfer { lamports }).expect("serialization cannot fail");

    Instruction {
        program_id: *SYSTEM_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*from, true),
            AccountMeta::new(*to, false),
        ],
        data,
    }
}

pub fn assign(account: &Hash, owner: &Hash) -> Instruction {
    let data = borsh::to_vec(&SystemInstruction::Assign { owner: *owner })
        .expect("serialization cannot fail");

    Instruction {
        program_id: *SYSTEM_PROGRAM_ID,
        accounts: vec![AccountMeta::new(*account, true)],
        data,
    }
}

pub fn allocate(account: &Hash, space: u64) -> Instruction {
    let data =
        borsh::to_vec(&SystemInstruction::Allocate { space }).expect("serialization cannot fail");

    Instruction {
        program_id: *SYSTEM_PROGRAM_ID,
        accounts: vec![AccountMeta::new(*account, true)],
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    #[test]
    fn system_instruction_borsh_roundtrip() {
        let instructions = vec![
            SystemInstruction::CreateAccount {
                lamports: 1000,
                space: 100,
                owner: hash(b"owner"),
            },
            SystemInstruction::Transfer { lamports: 500 },
            SystemInstruction::Assign {
                owner: hash(b"new_owner"),
            },
            SystemInstruction::Allocate { space: 200 },
            SystemInstruction::AdvanceNonceAccount,
            SystemInstruction::WithdrawNonceAccount(100),
            SystemInstruction::InitializeNonceAccount(hash(b"authority")),
            SystemInstruction::AuthorizeNonceAccount(hash(b"new_auth")),
        ];

        for ix in &instructions {
            let encoded = borsh::to_vec(ix).unwrap();
            let decoded: SystemInstruction = borsh::from_slice(&encoded).unwrap();
            assert_eq!(*ix, decoded);
        }
    }

    #[test]
    fn create_account_instruction() {
        let from = hash(b"from");
        let to = hash(b"to");
        let owner = hash(b"owner");
        let ix = create_account(&from, &to, 1000, 100, &owner);
        assert_eq!(ix.program_id, *SYSTEM_PROGRAM_ID);
        assert_eq!(ix.accounts.len(), 2);
        assert!(ix.accounts[0].is_signer);
        assert!(ix.accounts[1].is_signer);
    }

    #[test]
    fn transfer_instruction() {
        let from = hash(b"from");
        let to = hash(b"to");
        let ix = transfer(&from, &to, 500);
        assert_eq!(ix.program_id, *SYSTEM_PROGRAM_ID);
        assert_eq!(ix.accounts.len(), 2);

        let decoded: SystemInstruction = borsh::from_slice(&ix.data).unwrap();
        assert_eq!(decoded, SystemInstruction::Transfer { lamports: 500 });
    }
}
