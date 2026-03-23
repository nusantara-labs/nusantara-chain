//! Trait-based program processor abstraction and registry.
//!
//! [`ProgramProcessor`] provides a uniform interface for native program
//! processors. [`ProcessorRegistry`] replaces the hard-coded if-else chain
//! in `program_dispatch.rs` with a lookup table, making it trivial to add
//! new native programs without editing the dispatch function.

use std::collections::HashMap;

use nusantara_crypto::Hash;
use nusantara_vm::ProgramCache;

use crate::error::RuntimeError;
use crate::sysvar_cache::SysvarCache;
use crate::transaction_context::TransactionContext;

/// A native program processor that can handle instructions for a specific
/// program ID.
pub trait ProgramProcessor: Send + Sync {
    /// The program ID this processor handles.
    fn program_id(&self) -> &Hash;

    /// Process an instruction for this program.
    fn process(
        &self,
        accounts: &[u8],
        data: &[u8],
        ctx: &mut TransactionContext,
        sysvars: &SysvarCache,
        program_cache: &ProgramCache,
    ) -> Result<(), RuntimeError>;
}

/// Registry of native program processors, keyed by program ID.
pub struct ProcessorRegistry {
    processors: HashMap<Hash, Box<dyn ProgramProcessor>>,
}

impl ProcessorRegistry {
    /// Create a registry with all built-in native processors.
    pub fn new_with_defaults() -> Self {
        let mut registry = Self {
            processors: HashMap::new(),
        };
        registry.register(Box::new(SystemProcessor));
        registry.register(Box::new(StakeProcessor));
        registry.register(Box::new(VoteProcessor));
        registry.register(Box::new(ComputeBudgetProcessor));
        registry.register(Box::new(LoaderProcessor));
        registry.register(Box::new(TokenProcessor));
        registry
    }

    /// Register a new program processor.
    pub fn register(&mut self, processor: Box<dyn ProgramProcessor>) {
        self.processors.insert(*processor.program_id(), processor);
    }

    /// Look up a processor by program ID (O(1) via HashMap).
    pub fn find(&self, program_id: &Hash) -> Option<&dyn ProgramProcessor> {
        self.processors.get(program_id).map(|p| p.as_ref())
    }
}

// ---------------------------------------------------------------------------
// Built-in processor wrappers
// ---------------------------------------------------------------------------

use nusantara_core::program::{
    COMPUTE_BUDGET_PROGRAM_ID, LOADER_PROGRAM_ID, STAKE_PROGRAM_ID, SYSTEM_PROGRAM_ID,
    TOKEN_PROGRAM_ID, VOTE_PROGRAM_ID,
};

use crate::processors;

/// System program processor.
pub struct SystemProcessor;
impl ProgramProcessor for SystemProcessor {
    fn program_id(&self) -> &Hash {
        &SYSTEM_PROGRAM_ID
    }
    fn process(
        &self,
        accounts: &[u8],
        data: &[u8],
        ctx: &mut TransactionContext,
        sysvars: &SysvarCache,
        _program_cache: &ProgramCache,
    ) -> Result<(), RuntimeError> {
        processors::system::process_system(accounts, data, ctx, sysvars)
    }
}

/// Stake program processor.
pub struct StakeProcessor;
impl ProgramProcessor for StakeProcessor {
    fn program_id(&self) -> &Hash {
        &STAKE_PROGRAM_ID
    }
    fn process(
        &self,
        accounts: &[u8],
        data: &[u8],
        ctx: &mut TransactionContext,
        sysvars: &SysvarCache,
        _program_cache: &ProgramCache,
    ) -> Result<(), RuntimeError> {
        processors::stake::process_stake(accounts, data, ctx, sysvars)
    }
}

/// Vote program processor.
pub struct VoteProcessor;
impl ProgramProcessor for VoteProcessor {
    fn program_id(&self) -> &Hash {
        &VOTE_PROGRAM_ID
    }
    fn process(
        &self,
        accounts: &[u8],
        data: &[u8],
        ctx: &mut TransactionContext,
        sysvars: &SysvarCache,
        _program_cache: &ProgramCache,
    ) -> Result<(), RuntimeError> {
        processors::vote::process_vote(accounts, data, ctx, sysvars)
    }
}

/// Compute budget program processor.
pub struct ComputeBudgetProcessor;
impl ProgramProcessor for ComputeBudgetProcessor {
    fn program_id(&self) -> &Hash {
        &COMPUTE_BUDGET_PROGRAM_ID
    }
    fn process(
        &self,
        accounts: &[u8],
        data: &[u8],
        ctx: &mut TransactionContext,
        _sysvars: &SysvarCache,
        _program_cache: &ProgramCache,
    ) -> Result<(), RuntimeError> {
        processors::compute_budget::process_compute_budget(accounts, data, ctx)
    }
}

/// Loader program processor (BPF/WASM deploy/upgrade).
pub struct LoaderProcessor;
impl ProgramProcessor for LoaderProcessor {
    fn program_id(&self) -> &Hash {
        &LOADER_PROGRAM_ID
    }
    fn process(
        &self,
        accounts: &[u8],
        data: &[u8],
        ctx: &mut TransactionContext,
        sysvars: &SysvarCache,
        program_cache: &ProgramCache,
    ) -> Result<(), RuntimeError> {
        processors::loader::process_loader(accounts, data, ctx, sysvars, program_cache)
    }
}

/// Token program processor.
pub struct TokenProcessor;
impl ProgramProcessor for TokenProcessor {
    fn program_id(&self) -> &Hash {
        &TOKEN_PROGRAM_ID
    }
    fn process(
        &self,
        accounts: &[u8],
        data: &[u8],
        ctx: &mut TransactionContext,
        sysvars: &SysvarCache,
        _program_cache: &ProgramCache,
    ) -> Result<(), RuntimeError> {
        processors::token::process_token(accounts, data, ctx, sysvars)
    }
}
