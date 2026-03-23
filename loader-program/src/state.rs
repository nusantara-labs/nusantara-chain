use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::Hash;

/// State stored in loader-managed accounts.
///
/// Account types:
/// - Buffer: Temporary storage for bytecode during deployment
/// - Program: Executable account pointing to ProgramData
/// - ProgramData: Header + bytecode (the actual WASM code)
#[derive(Debug, Clone, BorshSerialize, BorshDeserialize, PartialEq, Eq)]
pub enum LoaderState {
    /// Account has not been initialized yet.
    Uninitialized,

    /// Buffer account: holds bytecode being written before deploy.
    Buffer {
        /// Authority who can write to this buffer and deploy from it.
        /// None means the buffer is orphaned (should be closed).
        authority: Option<Hash>,
    },

    /// Program account: an executable proxy pointing to its data account.
    /// The account itself is marked `executable = true`.
    /// Owner is LOADER_PROGRAM_ID.
    Program {
        /// Address of the ProgramData account containing the bytecode.
        program_data_address: Hash,
    },

    /// ProgramData account: header followed by WASM bytecode.
    /// Stored in account.data as: [LoaderState::ProgramData header] ++ [bytecode + padding]
    ProgramData {
        /// The slot at which the program was last deployed/upgraded.
        slot: u64,
        /// Authority who can upgrade this program.
        /// None means the program is immutable (no further upgrades).
        upgrade_authority: Option<Hash>,
        /// Actual bytecode length (excluding padding for future upgrades).
        bytecode_len: u64,
    },
}

impl LoaderState {
    /// Serialize this state to bytes.
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        borsh::to_vec(self).map_err(|e| e.to_string())
    }

    /// Deserialize state from the beginning of account data.
    pub fn from_account_data(data: &[u8]) -> Result<Self, String> {
        if data.is_empty() {
            return Ok(LoaderState::Uninitialized);
        }
        BorshDeserialize::deserialize(&mut &data[..]).map_err(|e| e.to_string())
    }

    /// For ProgramData accounts: get the header size (serialized LoaderState::ProgramData
    /// with `Some(Hash::zero())` as the upgrade authority).
    ///
    /// The bytecode starts immediately after this header in the account data.
    /// We compute the size from a canonical sample to avoid hardcoding magic numbers.
    pub fn program_data_header_size() -> usize {
        // ProgramData { slot: u64, upgrade_authority: Option<Hash>, bytecode_len: u64 }
        // borsh layout: enum variant (1) + slot (8) + option tag (1) + hash (64) + bytecode_len (8) = 82
        let sample = LoaderState::ProgramData {
            slot: 0,
            upgrade_authority: Some(Hash::zero()),
            bytecode_len: 0,
        };
        borsh::to_vec(&sample)
            .expect("canonical ProgramData serialization cannot fail")
            .len()
    }

    /// For ProgramData accounts: extract bytecode from account data.
    ///
    /// The data layout is: [serialized ProgramData header] ++ [bytecode bytes + padding].
    /// Uses the stored `bytecode_len` to return only the valid bytecode.
    pub fn extract_bytecode(data: &[u8]) -> Result<&[u8], String> {
        let state: LoaderState =
            BorshDeserialize::deserialize(&mut &data[..]).map_err(|e| e.to_string())?;

        match &state {
            LoaderState::ProgramData { bytecode_len, .. } => {
                let header_bytes = borsh::to_vec(&state).map_err(|e| e.to_string())?;
                let header_len = header_bytes.len();
                if header_len > data.len() {
                    return Err("account data too short for ProgramData header".to_string());
                }
                let bc_len = *bytecode_len as usize;
                let end = header_len + bc_len;
                if end > data.len() {
                    return Err("account data too short for bytecode".to_string());
                }
                Ok(&data[header_len..end])
            }
            _ => Err("not a ProgramData account".to_string()),
        }
    }

    /// Check if this state is Uninitialized.
    pub fn is_uninitialized(&self) -> bool {
        matches!(self, LoaderState::Uninitialized)
    }

    /// Check if this state is a Buffer.
    pub fn is_buffer(&self) -> bool {
        matches!(self, LoaderState::Buffer { .. })
    }

    /// Check if this state is a Program.
    pub fn is_program(&self) -> bool {
        matches!(self, LoaderState::Program { .. })
    }

    /// Check if this state is ProgramData.
    pub fn is_program_data(&self) -> bool {
        matches!(self, LoaderState::ProgramData { .. })
    }

    /// Get the authority if this is a Buffer or ProgramData.
    /// Returns `None` for Uninitialized and Program variants.
    pub fn authority(&self) -> Option<&Hash> {
        match self {
            LoaderState::Buffer { authority } => authority.as_ref(),
            LoaderState::ProgramData {
                upgrade_authority, ..
            } => upgrade_authority.as_ref(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    #[test]
    fn state_roundtrip() {
        let states = vec![
            LoaderState::Uninitialized,
            LoaderState::Buffer {
                authority: Some(hash(b"auth")),
            },
            LoaderState::Buffer { authority: None },
            LoaderState::Program {
                program_data_address: hash(b"pd_addr"),
            },
            LoaderState::ProgramData {
                slot: 42,
                upgrade_authority: Some(hash(b"upgrade_auth")),
                bytecode_len: 1024,
            },
            LoaderState::ProgramData {
                slot: 100,
                upgrade_authority: None,
                bytecode_len: 0,
            },
        ];

        for state in &states {
            let bytes = state.to_bytes().expect("serialize");
            let decoded = LoaderState::from_account_data(&bytes).expect("deserialize");
            assert_eq!(state, &decoded);
        }
    }

    #[test]
    fn empty_data_is_uninitialized() {
        let state = LoaderState::from_account_data(&[]).expect("empty data");
        assert!(state.is_uninitialized());
    }

    #[test]
    fn program_data_header_size_is_consistent() {
        let size = LoaderState::program_data_header_size();
        // enum variant (1) + slot (8) + option tag (1) + Hash (64) + bytecode_len (8) = 82
        assert_eq!(size, 82);
    }

    #[test]
    fn program_data_header_without_authority() {
        // Without authority: enum variant (1) + slot (8) + option tag (1) + bytecode_len (8) = 18
        let state = LoaderState::ProgramData {
            slot: 0,
            upgrade_authority: None,
            bytecode_len: 0,
        };
        let bytes = borsh::to_vec(&state).expect("serialize");
        assert_eq!(bytes.len(), 18);
    }

    #[test]
    fn extract_bytecode_works() {
        let bytecode = b"wasm bytecode here";
        let header = LoaderState::ProgramData {
            slot: 42,
            upgrade_authority: Some(hash(b"auth")),
            bytecode_len: bytecode.len() as u64,
        };
        let mut data = borsh::to_vec(&header).expect("serialize");
        data.extend_from_slice(bytecode);

        let extracted = LoaderState::extract_bytecode(&data).expect("extract");
        assert_eq!(extracted, bytecode);
    }

    #[test]
    fn extract_bytecode_empty_bytecode() {
        let header = LoaderState::ProgramData {
            slot: 0,
            upgrade_authority: None,
            bytecode_len: 0,
        };
        let data = borsh::to_vec(&header).expect("serialize");

        let extracted = LoaderState::extract_bytecode(&data).expect("extract");
        assert!(extracted.is_empty());
    }

    #[test]
    fn extract_bytecode_with_padding() {
        let bytecode = b"real wasm";
        let header = LoaderState::ProgramData {
            slot: 1,
            upgrade_authority: Some(hash(b"auth")),
            bytecode_len: bytecode.len() as u64,
        };
        let mut data = borsh::to_vec(&header).expect("serialize");
        data.extend_from_slice(bytecode);
        // Add padding
        data.resize(data.len() + 100, 0);

        let extracted = LoaderState::extract_bytecode(&data).expect("extract");
        assert_eq!(extracted, bytecode);
    }

    #[test]
    fn extract_bytecode_large_payload() {
        let wasm_bytes: Vec<u8> = (0u8..=255).cycle().take(65536).collect();
        let header = LoaderState::ProgramData {
            slot: 999,
            upgrade_authority: Some(hash(b"deployer")),
            bytecode_len: wasm_bytes.len() as u64,
        };
        let mut data = borsh::to_vec(&header).expect("serialize");
        data.extend_from_slice(&wasm_bytes);

        let extracted = LoaderState::extract_bytecode(&data).expect("extract");
        assert_eq!(extracted.len(), 65536);
        assert_eq!(extracted, wasm_bytes.as_slice());
    }

    #[test]
    fn extract_bytecode_wrong_type() {
        let header = LoaderState::Buffer {
            authority: Some(hash(b"auth")),
        };
        let data = borsh::to_vec(&header).expect("serialize");
        assert!(LoaderState::extract_bytecode(&data).is_err());
    }

    #[test]
    fn extract_bytecode_uninitialized() {
        let data = borsh::to_vec(&LoaderState::Uninitialized).expect("serialize");
        assert!(LoaderState::extract_bytecode(&data).is_err());
    }

    #[test]
    fn extract_bytecode_program_variant() {
        let state = LoaderState::Program {
            program_data_address: hash(b"pd"),
        };
        let data = borsh::to_vec(&state).expect("serialize");
        assert!(LoaderState::extract_bytecode(&data).is_err());
    }

    #[test]
    fn type_checks() {
        assert!(LoaderState::Uninitialized.is_uninitialized());
        assert!(!LoaderState::Uninitialized.is_buffer());
        assert!(!LoaderState::Uninitialized.is_program());
        assert!(!LoaderState::Uninitialized.is_program_data());

        let buffer = LoaderState::Buffer {
            authority: Some(hash(b"auth")),
        };
        assert!(buffer.is_buffer());
        assert!(!buffer.is_program());
        assert!(!buffer.is_uninitialized());
        assert!(!buffer.is_program_data());

        let program = LoaderState::Program {
            program_data_address: hash(b"pd"),
        };
        assert!(program.is_program());
        assert!(!program.is_program_data());
        assert!(!program.is_buffer());
        assert!(!program.is_uninitialized());

        let pd = LoaderState::ProgramData {
            slot: 0,
            upgrade_authority: None,
            bytecode_len: 0,
        };
        assert!(pd.is_program_data());
        assert!(!pd.is_program());
        assert!(!pd.is_buffer());
        assert!(!pd.is_uninitialized());
    }

    #[test]
    fn authority_extraction() {
        let auth = hash(b"auth");

        assert!(LoaderState::Uninitialized.authority().is_none());

        let buffer = LoaderState::Buffer {
            authority: Some(auth),
        };
        assert_eq!(buffer.authority(), Some(&auth));

        let buffer_none = LoaderState::Buffer { authority: None };
        assert!(buffer_none.authority().is_none());

        let pd = LoaderState::ProgramData {
            slot: 0,
            upgrade_authority: Some(auth),
            bytecode_len: 0,
        };
        assert_eq!(pd.authority(), Some(&auth));

        let pd_none = LoaderState::ProgramData {
            slot: 0,
            upgrade_authority: None,
            bytecode_len: 0,
        };
        assert!(pd_none.authority().is_none());

        let program = LoaderState::Program {
            program_data_address: hash(b"pd"),
        };
        assert!(program.authority().is_none());
    }

    #[test]
    fn invalid_data_returns_error() {
        let garbage = vec![255, 255, 255];
        assert!(LoaderState::from_account_data(&garbage).is_err());
    }

    #[test]
    fn buffer_with_and_without_authority_differ() {
        let with = LoaderState::Buffer {
            authority: Some(hash(b"auth")),
        };
        let without = LoaderState::Buffer { authority: None };
        assert_ne!(with, without);

        let with_bytes = with.to_bytes().expect("serialize");
        let without_bytes = without.to_bytes().expect("serialize");
        assert_ne!(with_bytes, without_bytes);
    }

    #[test]
    fn program_data_slot_is_preserved() {
        let state = LoaderState::ProgramData {
            slot: u64::MAX,
            upgrade_authority: None,
            bytecode_len: 0,
        };
        let bytes = state.to_bytes().expect("serialize");
        let decoded = LoaderState::from_account_data(&bytes).expect("deserialize");
        match decoded {
            LoaderState::ProgramData { slot, .. } => assert_eq!(slot, u64::MAX),
            _ => panic!("expected ProgramData"),
        }
    }
}
