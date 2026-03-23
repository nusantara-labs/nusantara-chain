use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::Hash;

/// Account state flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum AccountState {
    #[default]
    Uninitialized = 0,
    Initialized = 1,
    Frozen = 2,
}

/// A mint defines a token type.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Mint {
    /// The authority that can mint new tokens.
    pub mint_authority: Option<Hash>,
    /// Total supply of this token.
    pub supply: u64,
    /// Number of decimal places.
    pub decimals: u8,
    /// Is this mint initialized?
    pub is_initialized: bool,
    /// Optional authority that can freeze token accounts.
    pub freeze_authority: Option<Hash>,
}

impl Mint {
    pub const LEN: usize = 1 + 64 + 8 + 1 + 1 + 1 + 64;
}

/// A token account holding a balance of one token type.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct TokenAccount {
    /// The mint this account holds tokens for.
    pub mint: Hash,
    /// The owner of this token account.
    pub owner: Hash,
    /// Current token balance.
    pub amount: u64,
    /// Optional delegate address.
    pub delegate: Option<Hash>,
    /// Account state (initialized, frozen).
    pub state: AccountState,
    /// Amount delegated to the delegate.
    pub delegated_amount: u64,
    /// Optional close authority (defaults to owner).
    pub close_authority: Option<Hash>,
}

impl TokenAccount {
    pub const LEN: usize = 64 + 64 + 8 + 1 + 64 + 1 + 8 + 1 + 64;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_roundtrip() {
        let mint = Mint {
            mint_authority: Some(nusantara_crypto::hash(b"auth")),
            supply: 1_000_000,
            decimals: 9,
            is_initialized: true,
            freeze_authority: None,
        };
        let bytes = borsh::to_vec(&mint).unwrap();
        let decoded: Mint = borsh::from_slice(&bytes).unwrap();
        assert_eq!(mint, decoded);
    }

    #[test]
    fn token_account_roundtrip() {
        let acc = TokenAccount {
            mint: nusantara_crypto::hash(b"mint"),
            owner: nusantara_crypto::hash(b"owner"),
            amount: 500,
            delegate: None,
            state: AccountState::Initialized,
            delegated_amount: 0,
            close_authority: None,
        };
        let bytes = borsh::to_vec(&acc).unwrap();
        let decoded: TokenAccount = borsh::from_slice(&bytes).unwrap();
        assert_eq!(acc, decoded);
    }

    #[test]
    fn account_state_default_is_uninitialized() {
        assert_eq!(AccountState::default(), AccountState::Uninitialized);
    }
}
