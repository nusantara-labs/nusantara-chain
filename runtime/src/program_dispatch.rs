use std::sync::LazyLock;

use nusantara_core::program::{RENT_PROGRAM_ID, SYSVAR_PROGRAM_ID};
use nusantara_crypto::Hash;
use nusantara_vm::ProgramCache;

use crate::cost_schedule::{INSTRUCTION_BASE_COST, INSTRUCTION_DATA_BYTE_COST};
use crate::error::RuntimeError;
use crate::program_processor::ProcessorRegistry;
use crate::sysvar_cache::SysvarCache;
use crate::transaction_context::TransactionContext;
use crate::wasm_dispatch::dispatch_wasm_program;

// Re-export for backward compatibility (used by transaction_executor.rs)
pub use crate::cost_schedule::SIGNATURE_VERIFY_COST;

static REGISTRY: LazyLock<ProcessorRegistry> = LazyLock::new(ProcessorRegistry::new_with_defaults);

#[tracing::instrument(skip_all, fields(program_id = %program_id))]
pub fn dispatch_instruction(
    program_id: &Hash,
    accounts: &[u8],
    data: &[u8],
    ctx: &mut TransactionContext,
    sysvars: &SysvarCache,
    program_cache: &ProgramCache,
) -> Result<(), RuntimeError> {
    // Charge base instruction cost + data byte cost
    let base_cost = INSTRUCTION_BASE_COST + data.len() as u64 * INSTRUCTION_DATA_BYTE_COST;
    ctx.consume_compute(base_cost)?;

    // Reject non-executable built-in programs
    if *program_id == *SYSVAR_PROGRAM_ID || *program_id == *RENT_PROGRAM_ID {
        return Err(RuntimeError::UnknownProgram(
            "sysvar and rent programs are not executable".to_string(),
        ));
    }

    // Try native processor registry
    if let Some(processor) = REGISTRY.find(program_id) {
        return processor.process(accounts, data, ctx, sysvars, program_cache);
    }

    // Fallback to WASM dispatch
    dispatch_wasm_program(program_id, accounts, data, ctx, sysvars, program_cache)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_core::instruction::Instruction;
    use nusantara_core::program::{COMPUTE_BUDGET_PROGRAM_ID, SYSTEM_PROGRAM_ID};
    use nusantara_core::{Account, Message};
    use nusantara_crypto::hash;

    use crate::test_utils::{test_cache, test_sysvars};

    fn make_ctx_for_program(program_id: Hash) -> (TransactionContext, Vec<u8>, Vec<u8>) {
        let payer = hash(b"payer");
        let ix = Instruction {
            program_id,
            accounts: vec![],
            data: vec![],
        };
        let msg = Message::new(&[ix], &payer).unwrap();
        let accounts: Vec<_> = msg
            .account_keys
            .iter()
            .map(|k| (*k, Account::new(1000, hash(b"system"))))
            .collect();
        let compiled = msg.instructions[0].accounts.clone();
        let data = msg.instructions[0].data.clone();
        let ctx = TransactionContext::new(accounts, msg, 0, 100_000);
        (ctx, compiled, data)
    }

    #[test]
    fn dispatch_system() {
        let from = hash(b"from");
        let to = hash(b"to");
        let transfer_ix = nusantara_system_program::transfer(&from, &to, 100);
        let msg = Message::new(&[transfer_ix], &from).unwrap();
        let accounts: Vec<_> = msg
            .account_keys
            .iter()
            .map(|k| (*k, Account::new(1000, hash(b"system"))))
            .collect();
        let compiled = msg.instructions[0].accounts.clone();
        let data = msg.instructions[0].data.clone();
        let mut ctx = TransactionContext::new(accounts, msg, 0, 100_000);
        let sysvars = test_sysvars();
        let cache = test_cache();
        let pid = *SYSTEM_PROGRAM_ID;
        dispatch_instruction(&pid, &compiled, &data, &mut ctx, &sysvars, &cache).unwrap();
    }

    #[test]
    fn dispatch_compute_budget() {
        let (mut ctx, compiled, data) = make_ctx_for_program(*COMPUTE_BUDGET_PROGRAM_ID);
        let sysvars = test_sysvars();
        let cache = test_cache();
        let pid = *COMPUTE_BUDGET_PROGRAM_ID;
        dispatch_instruction(&pid, &compiled, &data, &mut ctx, &sysvars, &cache).unwrap();
    }

    #[test]
    fn dispatch_sysvar_fails() {
        let (mut ctx, compiled, data) = make_ctx_for_program(*SYSVAR_PROGRAM_ID);
        let sysvars = test_sysvars();
        let cache = test_cache();
        let pid = *SYSVAR_PROGRAM_ID;
        let err =
            dispatch_instruction(&pid, &compiled, &data, &mut ctx, &sysvars, &cache).unwrap_err();
        assert!(matches!(err, RuntimeError::UnknownProgram(_)));
    }

    #[test]
    fn dispatch_unknown() {
        let unknown = hash(b"unknown_program");
        let (mut ctx, compiled, data) = make_ctx_for_program(unknown);
        let sysvars = test_sysvars();
        let cache = test_cache();
        let err = dispatch_instruction(&unknown, &compiled, &data, &mut ctx, &sysvars, &cache)
            .unwrap_err();
        // Unknown programs now try WASM dispatch first, which fails with ProgramNotExecutable
        assert!(
            matches!(err, RuntimeError::ProgramNotExecutable(_))
                || matches!(err, RuntimeError::UnknownProgram(_))
        );
    }

    #[test]
    fn dispatch_charges_base_cost() {
        let (mut ctx, compiled, _) = make_ctx_for_program(*COMPUTE_BUDGET_PROGRAM_ID);
        let data = vec![0u8; 10]; // 10 bytes of data
        let sysvars = test_sysvars();
        let cache = test_cache();
        let before = ctx.compute_remaining();
        let pid = *COMPUTE_BUDGET_PROGRAM_ID;
        dispatch_instruction(&pid, &compiled, &data, &mut ctx, &sysvars, &cache).unwrap();
        let consumed = before - ctx.compute_remaining();
        // INSTRUCTION_BASE_COST + 10 * INSTRUCTION_DATA_BYTE_COST = 200 + 10 = 210
        assert_eq!(consumed, 210);
    }
}
