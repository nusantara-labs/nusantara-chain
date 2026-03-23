//! Centralized compute cost constants for all native programs.
//!
//! Every instruction charges compute units from the transaction's
//! [`ComputeMeter`](crate::compute_meter::ComputeMeter). This module is the
//! single source of truth for all cost constants, replacing scattered `const`
//! definitions across individual processor files and `program_dispatch`.

// ---------------------------------------------------------------------------
// Global (charged by program_dispatch for every instruction)
// ---------------------------------------------------------------------------

/// Base cost charged for every instruction dispatch.
pub const INSTRUCTION_BASE_COST: u64 = 200;

/// Per-byte cost for instruction data.
pub const INSTRUCTION_DATA_BYTE_COST: u64 = 1;

/// Cost per required signature verification.
pub const SIGNATURE_VERIFY_COST: u64 = 2000;

// ---------------------------------------------------------------------------
// System program
// ---------------------------------------------------------------------------

pub const SYSTEM_CREATE_ACCOUNT_COST: u64 = 1500;
pub const SYSTEM_TRANSFER_COST: u64 = 450;
pub const SYSTEM_ASSIGN_COST: u64 = 450;
pub const SYSTEM_ALLOCATE_COST: u64 = 450;

// ---------------------------------------------------------------------------
// Stake program
// ---------------------------------------------------------------------------

pub const STAKE_BASE_COST: u64 = 750;

// ---------------------------------------------------------------------------
// Vote program
// ---------------------------------------------------------------------------

pub const VOTE_BASE_COST: u64 = 2100;

// ---------------------------------------------------------------------------
// Token program
// ---------------------------------------------------------------------------

pub const TOKEN_INIT_MINT_COST: u64 = 1000;
pub const TOKEN_INIT_ACCOUNT_COST: u64 = 1000;
pub const TOKEN_MINT_TO_COST: u64 = 500;
pub const TOKEN_TRANSFER_COST: u64 = 300;
pub const TOKEN_APPROVE_COST: u64 = 300;
pub const TOKEN_REVOKE_COST: u64 = 300;
pub const TOKEN_BURN_COST: u64 = 500;
pub const TOKEN_CLOSE_COST: u64 = 500;
pub const TOKEN_FREEZE_COST: u64 = 300;
pub const TOKEN_THAW_COST: u64 = 300;

// ---------------------------------------------------------------------------
// Loader program
// ---------------------------------------------------------------------------

pub const LOADER_INITIALIZE_BUFFER_COST: u64 = 500;
pub const LOADER_WRITE_COST: u64 = 200;
pub const LOADER_DEPLOY_COST: u64 = 5000;
pub const LOADER_UPGRADE_COST: u64 = 5000;
pub const LOADER_SET_AUTHORITY_COST: u64 = 500;
pub const LOADER_CLOSE_COST: u64 = 500;
