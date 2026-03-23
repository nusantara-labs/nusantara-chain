pub mod error;
pub mod state;

use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_core::instruction::{AccountMeta, Instruction};
use nusantara_core::program::TOKEN_PROGRAM_ID;
use nusantara_crypto::Hash;

/// Token program instructions.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum TokenInstruction {
    /// Initialize a new mint.
    /// Accounts: [writable] mint, [] rent_sysvar
    InitializeMint {
        decimals: u8,
        mint_authority: Hash,
        freeze_authority: Option<Hash>,
    },

    /// Initialize a new token account.
    /// Accounts: [writable] account, [] mint, [] owner, [] rent_sysvar
    InitializeAccount,

    /// Mint tokens to an account.
    /// Accounts: [writable] mint, [writable] destination, [signer] mint_authority
    MintTo { amount: u64 },

    /// Transfer tokens between accounts.
    /// Accounts: [writable] source, [writable] destination, [signer] authority
    Transfer { amount: u64 },

    /// Approve a delegate.
    /// Accounts: [writable] source, [] delegate, [signer] owner
    Approve { amount: u64 },

    /// Revoke a delegate.
    /// Accounts: [writable] source, [signer] owner
    Revoke,

    /// Burn tokens.
    /// Accounts: [writable] source, [writable] mint, [signer] authority
    Burn { amount: u64 },

    /// Close a token account.
    /// Accounts: [writable] account, [writable] destination, [signer] authority
    CloseAccount,

    /// Freeze a token account.
    /// Accounts: [writable] account, [] mint, [signer] freeze_authority
    FreezeAccount,

    /// Thaw a frozen token account.
    /// Accounts: [writable] account, [] mint, [signer] freeze_authority
    ThawAccount,
}

// Instruction constructors

pub fn initialize_mint(
    mint: &Hash,
    decimals: u8,
    mint_authority: &Hash,
    freeze_authority: Option<&Hash>,
) -> Instruction {
    let data = borsh::to_vec(&TokenInstruction::InitializeMint {
        decimals,
        mint_authority: *mint_authority,
        freeze_authority: freeze_authority.copied(),
    })
    .expect("serialization cannot fail");

    Instruction {
        program_id: *TOKEN_PROGRAM_ID,
        accounts: vec![AccountMeta::new(*mint, false)],
        data,
    }
}

pub fn initialize_account(account: &Hash, mint: &Hash, owner: &Hash) -> Instruction {
    let data =
        borsh::to_vec(&TokenInstruction::InitializeAccount).expect("serialization cannot fail");

    Instruction {
        program_id: *TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*account, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(*owner, false),
        ],
        data,
    }
}

pub fn mint_to(mint: &Hash, destination: &Hash, mint_authority: &Hash, amount: u64) -> Instruction {
    let data =
        borsh::to_vec(&TokenInstruction::MintTo { amount }).expect("serialization cannot fail");

    Instruction {
        program_id: *TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*mint, false),
            AccountMeta::new(*destination, false),
            AccountMeta::new_readonly(*mint_authority, true),
        ],
        data,
    }
}

pub fn transfer(source: &Hash, destination: &Hash, authority: &Hash, amount: u64) -> Instruction {
    let data =
        borsh::to_vec(&TokenInstruction::Transfer { amount }).expect("serialization cannot fail");

    Instruction {
        program_id: *TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*source, false),
            AccountMeta::new(*destination, false),
            AccountMeta::new_readonly(*authority, true),
        ],
        data,
    }
}

pub fn approve(source: &Hash, delegate: &Hash, owner: &Hash, amount: u64) -> Instruction {
    let data =
        borsh::to_vec(&TokenInstruction::Approve { amount }).expect("serialization cannot fail");

    Instruction {
        program_id: *TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*source, false),
            AccountMeta::new_readonly(*delegate, false),
            AccountMeta::new_readonly(*owner, true),
        ],
        data,
    }
}

pub fn revoke(source: &Hash, owner: &Hash) -> Instruction {
    let data = borsh::to_vec(&TokenInstruction::Revoke).expect("serialization cannot fail");

    Instruction {
        program_id: *TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*source, false),
            AccountMeta::new_readonly(*owner, true),
        ],
        data,
    }
}

pub fn burn(source: &Hash, mint: &Hash, authority: &Hash, amount: u64) -> Instruction {
    let data =
        borsh::to_vec(&TokenInstruction::Burn { amount }).expect("serialization cannot fail");

    Instruction {
        program_id: *TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*source, false),
            AccountMeta::new(*mint, false),
            AccountMeta::new_readonly(*authority, true),
        ],
        data,
    }
}

pub fn close_account(account: &Hash, destination: &Hash, authority: &Hash) -> Instruction {
    let data = borsh::to_vec(&TokenInstruction::CloseAccount).expect("serialization cannot fail");

    Instruction {
        program_id: *TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*account, false),
            AccountMeta::new(*destination, false),
            AccountMeta::new_readonly(*authority, true),
        ],
        data,
    }
}

pub fn freeze_account(account: &Hash, mint: &Hash, freeze_authority: &Hash) -> Instruction {
    let data = borsh::to_vec(&TokenInstruction::FreezeAccount).expect("serialization cannot fail");

    Instruction {
        program_id: *TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*account, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(*freeze_authority, true),
        ],
        data,
    }
}

pub fn thaw_account(account: &Hash, mint: &Hash, freeze_authority: &Hash) -> Instruction {
    let data = borsh::to_vec(&TokenInstruction::ThawAccount).expect("serialization cannot fail");

    Instruction {
        program_id: *TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*account, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(*freeze_authority, true),
        ],
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_roundtrip() {
        let cases: Vec<TokenInstruction> = vec![
            TokenInstruction::InitializeMint {
                decimals: 9,
                mint_authority: nusantara_crypto::hash(b"auth"),
                freeze_authority: None,
            },
            TokenInstruction::InitializeAccount,
            TokenInstruction::MintTo { amount: 1000 },
            TokenInstruction::Transfer { amount: 500 },
            TokenInstruction::Approve { amount: 200 },
            TokenInstruction::Revoke,
            TokenInstruction::Burn { amount: 100 },
            TokenInstruction::CloseAccount,
            TokenInstruction::FreezeAccount,
            TokenInstruction::ThawAccount,
        ];

        for ix in cases {
            let bytes = borsh::to_vec(&ix).unwrap();
            let decoded: TokenInstruction = borsh::from_slice(&bytes).unwrap();
            assert_eq!(ix, decoded);
        }
    }
}
