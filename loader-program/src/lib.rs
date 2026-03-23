pub mod state;

use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_core::instruction::{AccountMeta, Instruction};
use nusantara_core::program::LOADER_PROGRAM_ID;
use nusantara_crypto::Hash;

/// Instructions for the program loader.
#[derive(Debug, Clone, BorshSerialize, BorshDeserialize, PartialEq, Eq)]
pub enum LoaderInstruction {
    /// Initialize a buffer account for writing bytecode.
    /// Accounts:
    ///   0. [writable, signer] Buffer account
    ///   1. [] Authority (must be signer in the transaction)
    InitializeBuffer,

    /// Write bytecode to buffer at the given offset.
    /// Accounts:
    ///   0. [writable] Buffer account
    ///   1. [signer] Authority
    Write { offset: u32, data: Vec<u8> },

    /// Deploy a program from a buffer.
    /// Validates WASM, creates Program + ProgramData accounts, closes buffer.
    /// Accounts:
    ///   0. [writable, signer] Payer (pays for ProgramData account)
    ///   1. [writable] Program account (will be set to executable)
    ///   2. [writable] ProgramData account (stores header + bytecode)
    ///   3. [writable] Buffer account (will be closed)
    ///   4. [signer] Authority (of buffer)
    Deploy { max_data_len: u64 },

    /// Upgrade a deployed program with new bytecode from a buffer.
    /// Accounts:
    ///   0. [writable] Program account
    ///   1. [writable] ProgramData account
    ///   2. [writable] Buffer account (will be closed)
    ///   3. [signer] Upgrade authority
    Upgrade,

    /// Set or revoke the authority for a buffer or program.
    /// Accounts:
    ///   0. [writable] Buffer or ProgramData account
    ///   1. [signer] Current authority
    ///   2. [] New authority (optional -- omit to make immutable)
    SetAuthority { new_authority: Option<Hash> },

    /// Close a buffer or program, reclaiming lamports.
    /// Accounts:
    ///   0. [writable] Account to close (buffer or program)
    ///   1. [writable] Recipient of lamports
    ///   2. [signer] Authority (if buffer/program_data)
    Close,
}

/// Build an InitializeBuffer instruction.
pub fn initialize_buffer(buffer: &Hash, authority: &Hash) -> Instruction {
    Instruction {
        program_id: *LOADER_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*buffer, true),
            AccountMeta::new_readonly(*authority, true),
        ],
        data: borsh::to_vec(&LoaderInstruction::InitializeBuffer)
            .expect("serialization cannot fail"),
    }
}

/// Build a Write instruction.
pub fn write(buffer: &Hash, authority: &Hash, offset: u32, data: Vec<u8>) -> Instruction {
    Instruction {
        program_id: *LOADER_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*buffer, false),
            AccountMeta::new_readonly(*authority, true),
        ],
        data: borsh::to_vec(&LoaderInstruction::Write { offset, data })
            .expect("serialization cannot fail"),
    }
}

/// Build a Deploy instruction.
pub fn deploy(
    payer: &Hash,
    program: &Hash,
    program_data: &Hash,
    buffer: &Hash,
    authority: &Hash,
    max_data_len: u64,
) -> Instruction {
    Instruction {
        program_id: *LOADER_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(*program, true),
            AccountMeta::new(*program_data, false),
            AccountMeta::new(*buffer, false),
            AccountMeta::new_readonly(*authority, true),
        ],
        data: borsh::to_vec(&LoaderInstruction::Deploy { max_data_len })
            .expect("serialization cannot fail"),
    }
}

/// Build an Upgrade instruction.
pub fn upgrade(
    program: &Hash,
    program_data: &Hash,
    buffer: &Hash,
    authority: &Hash,
) -> Instruction {
    Instruction {
        program_id: *LOADER_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*program, false),
            AccountMeta::new(*program_data, false),
            AccountMeta::new(*buffer, false),
            AccountMeta::new_readonly(*authority, true),
        ],
        data: borsh::to_vec(&LoaderInstruction::Upgrade).expect("serialization cannot fail"),
    }
}

/// Build a SetAuthority instruction.
pub fn set_authority(
    account: &Hash,
    current_authority: &Hash,
    new_authority: Option<Hash>,
) -> Instruction {
    let mut accounts = vec![
        AccountMeta::new(*account, false),
        AccountMeta::new_readonly(*current_authority, true),
    ];
    if let Some(new_auth) = &new_authority {
        accounts.push(AccountMeta::new_readonly(*new_auth, false));
    }
    Instruction {
        program_id: *LOADER_PROGRAM_ID,
        accounts,
        data: borsh::to_vec(&LoaderInstruction::SetAuthority { new_authority })
            .expect("serialization cannot fail"),
    }
}

/// Build a Close instruction.
pub fn close(account: &Hash, recipient: &Hash, authority: &Hash) -> Instruction {
    Instruction {
        program_id: *LOADER_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*account, false),
            AccountMeta::new(*recipient, false),
            AccountMeta::new_readonly(*authority, true),
        ],
        data: borsh::to_vec(&LoaderInstruction::Close).expect("serialization cannot fail"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    #[test]
    fn instruction_roundtrip() {
        let instructions = vec![
            LoaderInstruction::InitializeBuffer,
            LoaderInstruction::Write {
                offset: 100,
                data: vec![1, 2, 3],
            },
            LoaderInstruction::Deploy { max_data_len: 1024 },
            LoaderInstruction::Upgrade,
            LoaderInstruction::SetAuthority {
                new_authority: Some(hash(b"new_auth")),
            },
            LoaderInstruction::SetAuthority {
                new_authority: None,
            },
            LoaderInstruction::Close,
        ];

        for ix in &instructions {
            let encoded = borsh::to_vec(ix).expect("serialize");
            let decoded: LoaderInstruction = borsh::from_slice(&encoded).expect("deserialize");
            assert_eq!(ix, &decoded);
        }
    }

    #[test]
    fn initialize_buffer_instruction() {
        let buffer = hash(b"buffer");
        let authority = hash(b"authority");
        let ix = initialize_buffer(&buffer, &authority);
        assert_eq!(ix.program_id, *LOADER_PROGRAM_ID);
        assert_eq!(ix.accounts.len(), 2);
        assert!(ix.accounts[0].is_signer);
        assert!(ix.accounts[0].is_writable);
        assert!(ix.accounts[1].is_signer);
        assert!(!ix.accounts[1].is_writable);
    }

    #[test]
    fn write_instruction() {
        let buffer = hash(b"buffer");
        let authority = hash(b"authority");
        let ix = write(&buffer, &authority, 0, vec![1, 2, 3]);
        let decoded: LoaderInstruction = borsh::from_slice(&ix.data).expect("deserialize");
        assert!(
            matches!(decoded, LoaderInstruction::Write { offset: 0, data } if data == vec![1, 2, 3])
        );
    }

    #[test]
    fn deploy_instruction() {
        let payer = hash(b"payer");
        let program = hash(b"program");
        let program_data = hash(b"program_data");
        let buffer = hash(b"buffer");
        let authority = hash(b"authority");
        let ix = deploy(&payer, &program, &program_data, &buffer, &authority, 2048);
        assert_eq!(ix.accounts.len(), 5);
        let decoded: LoaderInstruction = borsh::from_slice(&ix.data).expect("deserialize");
        assert!(matches!(
            decoded,
            LoaderInstruction::Deploy { max_data_len: 2048 }
        ));
    }

    #[test]
    fn upgrade_instruction() {
        let program = hash(b"program");
        let program_data = hash(b"program_data");
        let buffer = hash(b"buffer");
        let authority = hash(b"authority");
        let ix = upgrade(&program, &program_data, &buffer, &authority);
        assert_eq!(ix.accounts.len(), 4);
        let decoded: LoaderInstruction = borsh::from_slice(&ix.data).expect("deserialize");
        assert!(matches!(decoded, LoaderInstruction::Upgrade));
    }

    #[test]
    fn set_authority_with_new() {
        let account = hash(b"account");
        let current = hash(b"current");
        let new_auth = hash(b"new");
        let ix = set_authority(&account, &current, Some(new_auth));
        assert_eq!(ix.accounts.len(), 3);
        let decoded: LoaderInstruction = borsh::from_slice(&ix.data).expect("deserialize");
        assert!(
            matches!(decoded, LoaderInstruction::SetAuthority { new_authority: Some(a) } if a == new_auth)
        );
    }

    #[test]
    fn set_authority_revoke() {
        let account = hash(b"account");
        let current = hash(b"current");
        let ix = set_authority(&account, &current, None);
        assert_eq!(ix.accounts.len(), 2);
        let decoded: LoaderInstruction = borsh::from_slice(&ix.data).expect("deserialize");
        assert!(matches!(
            decoded,
            LoaderInstruction::SetAuthority {
                new_authority: None
            }
        ));
    }

    #[test]
    fn close_instruction() {
        let account = hash(b"account");
        let recipient = hash(b"recipient");
        let authority = hash(b"authority");
        let ix = close(&account, &recipient, &authority);
        assert_eq!(ix.accounts.len(), 3);
        assert!(ix.accounts[0].is_writable);
        assert!(ix.accounts[1].is_writable);
        assert!(!ix.accounts[2].is_writable);
        assert!(ix.accounts[2].is_signer);
    }

    #[test]
    fn write_preserves_data_fidelity() {
        let buffer = hash(b"buffer");
        let authority = hash(b"authority");
        // Test with larger data chunk simulating WASM bytecode
        let payload: Vec<u8> = (0..=255).collect();
        let ix = write(&buffer, &authority, 512, payload.clone());
        let decoded: LoaderInstruction = borsh::from_slice(&ix.data).expect("deserialize");
        match decoded {
            LoaderInstruction::Write { offset, data } => {
                assert_eq!(offset, 512);
                assert_eq!(data, payload);
            }
            _ => panic!("expected Write instruction"),
        }
    }

    #[test]
    fn deploy_account_roles() {
        let payer = hash(b"payer");
        let program = hash(b"program");
        let program_data = hash(b"program_data");
        let buffer = hash(b"buffer");
        let authority = hash(b"authority");
        let ix = deploy(&payer, &program, &program_data, &buffer, &authority, 4096);

        // Payer: writable + signer
        assert!(ix.accounts[0].is_writable);
        assert!(ix.accounts[0].is_signer);
        // Program: writable + signer
        assert!(ix.accounts[1].is_writable);
        assert!(ix.accounts[1].is_signer);
        // ProgramData: writable, not signer
        assert!(ix.accounts[2].is_writable);
        assert!(!ix.accounts[2].is_signer);
        // Buffer: writable, not signer
        assert!(ix.accounts[3].is_writable);
        assert!(!ix.accounts[3].is_signer);
        // Authority: readonly, signer
        assert!(!ix.accounts[4].is_writable);
        assert!(ix.accounts[4].is_signer);
    }
}
