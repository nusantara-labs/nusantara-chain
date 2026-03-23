//! Error types returned by Nusantara smart contracts.
//!
//! Programs return `ProgramResult` from their handler functions. On success the
//! VM commits account state changes; on error it reverts them and records the
//! error code in the transaction receipt.

/// Errors that a program can return to the VM.
///
/// Each variant maps to a numeric code via [`ProgramError::to_code`]. The VM
/// stores this code in the transaction status and makes it available to clients.
/// Programs can also return arbitrary codes via `Custom(u32)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramError {
    /// Custom error with a program-defined code.
    Custom(u32),
    /// Instruction data could not be decoded.
    InvalidInstructionData,
    /// The accounts slice is shorter than expected.
    NotEnoughAccountKeys,
    /// A required signer did not sign the transaction.
    MissingRequiredSignature,
    /// An account that must be writable was passed as read-only.
    AccountNotWritable,
    /// Account data is too small for the operation.
    AccountDataTooSmall,
    /// The payer or source account has insufficient lamports.
    InsufficientFunds,
    /// Attempted to initialize an account that is already initialized.
    AccountAlreadyInitialized,
    /// Attempted to use an account that has not been initialized.
    UninitializedAccount,
    /// Account data failed validation or deserialization.
    InvalidAccountData,
    /// The account's owner does not match the expected program.
    InvalidAccountOwner,
    /// Borsh serialization or deserialization failed.
    BorshIoError(String),
}

/// Result type for program handler functions.
pub type ProgramResult = Result<(), ProgramError>;

impl ProgramError {
    /// Convert to a numeric error code for the VM.
    ///
    /// `Custom` codes are returned as-is (cast to `u64`). Built-in errors use
    /// fixed codes starting at 1.
    pub fn to_code(&self) -> u64 {
        match self {
            ProgramError::Custom(code) => u64::from(*code),
            ProgramError::InvalidInstructionData => 1,
            ProgramError::NotEnoughAccountKeys => 2,
            ProgramError::MissingRequiredSignature => 3,
            ProgramError::AccountNotWritable => 4,
            ProgramError::AccountDataTooSmall => 5,
            ProgramError::InsufficientFunds => 6,
            ProgramError::AccountAlreadyInitialized => 7,
            ProgramError::UninitializedAccount => 8,
            ProgramError::InvalidAccountData => 9,
            ProgramError::InvalidAccountOwner => 10,
            ProgramError::BorshIoError(_) => 11,
        }
    }
}

impl core::fmt::Display for ProgramError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ProgramError::Custom(code) => write!(f, "custom program error: {code}"),
            ProgramError::InvalidInstructionData => write!(f, "invalid instruction data"),
            ProgramError::NotEnoughAccountKeys => write!(f, "not enough account keys"),
            ProgramError::MissingRequiredSignature => write!(f, "missing required signature"),
            ProgramError::AccountNotWritable => write!(f, "account not writable"),
            ProgramError::AccountDataTooSmall => write!(f, "account data too small"),
            ProgramError::InsufficientFunds => write!(f, "insufficient funds"),
            ProgramError::AccountAlreadyInitialized => write!(f, "account already initialized"),
            ProgramError::UninitializedAccount => write!(f, "uninitialized account"),
            ProgramError::InvalidAccountData => write!(f, "invalid account data"),
            ProgramError::InvalidAccountOwner => write!(f, "invalid account owner"),
            ProgramError::BorshIoError(e) => write!(f, "borsh io error: {e}"),
        }
    }
}

impl From<std::io::Error> for ProgramError {
    fn from(e: std::io::Error) -> Self {
        ProgramError::BorshIoError(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(ProgramError::InvalidInstructionData.to_code(), 1);
        assert_eq!(ProgramError::NotEnoughAccountKeys.to_code(), 2);
        assert_eq!(ProgramError::MissingRequiredSignature.to_code(), 3);
        assert_eq!(ProgramError::AccountNotWritable.to_code(), 4);
        assert_eq!(ProgramError::AccountDataTooSmall.to_code(), 5);
        assert_eq!(ProgramError::InsufficientFunds.to_code(), 6);
        assert_eq!(ProgramError::AccountAlreadyInitialized.to_code(), 7);
        assert_eq!(ProgramError::UninitializedAccount.to_code(), 8);
        assert_eq!(ProgramError::InvalidAccountData.to_code(), 9);
        assert_eq!(ProgramError::InvalidAccountOwner.to_code(), 10);
        assert_eq!(ProgramError::BorshIoError("x".into()).to_code(), 11);
    }

    #[test]
    fn custom_error_code() {
        assert_eq!(ProgramError::Custom(42).to_code(), 42);
        assert_eq!(ProgramError::Custom(0).to_code(), 0);
        assert_eq!(
            ProgramError::Custom(u32::MAX).to_code(),
            u64::from(u32::MAX)
        );
    }

    #[test]
    fn error_display() {
        assert_eq!(
            ProgramError::InsufficientFunds.to_string(),
            "insufficient funds"
        );
        assert_eq!(
            ProgramError::Custom(99).to_string(),
            "custom program error: 99"
        );
        assert_eq!(
            ProgramError::BorshIoError("bad data".into()).to_string(),
            "borsh io error: bad data"
        );
    }

    #[test]
    fn error_from_io_error() {
        let io_err = std::io::Error::other("disk failure");
        let prog_err: ProgramError = io_err.into();
        assert!(
            matches!(prog_err, ProgramError::BorshIoError(ref s) if s.contains("disk failure"))
        );
    }

    #[test]
    fn error_equality() {
        assert_eq!(ProgramError::Custom(1), ProgramError::Custom(1));
        assert_ne!(ProgramError::Custom(1), ProgramError::Custom(2));
        assert_ne!(
            ProgramError::Custom(1),
            ProgramError::InvalidInstructionData
        );
    }
}
