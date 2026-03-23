//! Nusantara smart contract SDK.
//!
//! This crate provides the types, macros, and syscall bindings needed to write
//! WASM smart contracts for the Nusantara blockchain. It is standalone and does
//! NOT depend on `nusantara-crypto` or `nusantara-core`, so that it can be
//! compiled to `wasm32-unknown-unknown` without pulling in the full node stack.
//!
//! # Architecture
//!
//! - [`pubkey`]: 64-byte public key / hash type matching `nusantara_crypto::Hash`
//! - [`account_info`]: Account metadata passed to programs by the VM
//! - [`program_error`]: Error enum and `ProgramResult` alias
//! - [`program`]: Cross-program invocation helpers
//! - [`log`]: Logging via the `msg!` macro and `nusa_log` syscall
//! - [`sysvar`]: Clock, Rent, and EpochSchedule sysvar accessors
//! - [`entrypoint`]: The `entrypoint!` macro for defining the WASM entry point
//! - [`syscall`]: Raw `extern "C"` declarations for `nusa_*` host functions
//!
//! # Quick start
//!
//! ```ignore
//! use nusantara_sdk::prelude::*;
//!
//! entrypoint!(process_instruction);
//!
//! fn process_instruction(
//!     program_id: &Pubkey,
//!     accounts: &[AccountInfo],
//!     data: &[u8],
//! ) -> ProgramResult {
//!     msg!("Hello from Nusantara!");
//!     Ok(())
//! }
//! ```

pub mod account_info;
pub mod entrypoint;
pub mod log;
pub mod program;
pub mod program_error;
pub mod pubkey;
pub mod syscall;
pub mod sysvar;

/// Prelude for convenient imports.
pub mod prelude {
    pub use crate::account_info::AccountInfo;
    pub use crate::entrypoint;
    pub use crate::msg;
    pub use crate::program::{invoke, invoke_signed};
    pub use crate::program_error::{ProgramError, ProgramResult};
    pub use crate::pubkey::Pubkey;
    pub use nusantara_sdk_macro::{Accounts, program};
}
