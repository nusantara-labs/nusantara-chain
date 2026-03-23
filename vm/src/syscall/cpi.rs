//! Cross-program invocation (CPI) syscall.
//!
//! CPI allows a WASM program to invoke another on-chain program. The VM
//! enforces several safety invariants:
//!
//! 1. **Depth limit** -- CPI nesting cannot exceed [`MAX_CPI_DEPTH`].
//! 2. **Reentrancy** -- a program cannot appear twice in the call stack.
//! 3. **Privilege escalation** -- all accounts passed to the callee must
//!    already be present in the caller's account list with at least the
//!    same privileges.
//! 4. **Compute metering** -- each CPI incurs a base cost, and the callee
//!    shares the same compute budget as the caller.

use nusantara_crypto::{Hash, create_program_address};

use crate::config::COST_CPI_BASE;
use crate::error::VmError;
use crate::host_state::VmHostState;

/// Process a cross-program invocation.
///
/// # Steps
///
/// 1. Charge the base CPI compute cost.
/// 2. Validate that the CPI depth limit has not been reached.
/// 3. Check that the target program is not already on the call stack
///    (reentrancy prevention).
/// 4. Validate that all account indices are within bounds.
/// 5. Call the runtime-supplied dispatch function.
/// 6. Clean up the call stack on return.
pub fn invoke_cross_program(
    target_program_id: &Hash,
    account_indices: &[usize],
    instruction_data: &[u8],
    host_state: &mut VmHostState<'_>,
) -> Result<(), VmError> {
    // Charge CPI base cost
    host_state.consume_compute(COST_CPI_BASE)?;

    // Depth check
    host_state.check_cpi_depth()?;

    // Reentrancy check
    host_state.check_reentrancy(target_program_id)?;

    // Validate that all requested account indices are in bounds
    for &idx in account_indices {
        if idx >= host_state.account_privileges.len() {
            return Err(VmError::AccountNotFound(idx));
        }
    }

    // Obtain dispatch function -- CPI is unavailable without one
    let dispatch_fn = host_state
        .dispatch_fn
        .ok_or_else(|| VmError::Syscall("CPI not available: no dispatch function".to_string()))?;

    // Push target onto the call stack and increment depth
    host_state.call_stack.push(*target_program_id);
    let new_depth = host_state.cpi_depth + 1;

    // Invoke the target program via the runtime dispatch
    let result = dispatch_fn(
        target_program_id,
        account_indices,
        instruction_data,
        host_state.accounts,
        host_state.account_privileges,
        &mut host_state.compute_remaining,
        host_state.slot,
        host_state.program_cache,
        new_depth,
        &mut host_state.call_stack,
    );

    // Pop the call stack regardless of success/failure
    host_state.call_stack.pop();

    result.map_err(|e| VmError::Syscall(format!("CPI failed: {e}")))
}

/// Fixed compute-unit cost for a CPI invocation.
pub fn cpi_cost() -> u64 {
    COST_CPI_BASE
}

/// Validate that each claimed PDA signer was legitimately derived from the
/// calling program's ID.
///
/// During `invoke_signed`, programs provide seed sets that should derive to
/// account addresses in the transaction. This function verifies each claim
/// by re-deriving the PDA and checking it matches the expected address.
///
/// # Parameters
///
/// - `signer_seeds`: each element is a list of seed slices that should derive
///   to a PDA when combined with `calling_program_id`.
/// - `calling_program_id`: the program ID of the caller (used as the base for
///   PDA derivation).
/// - `expected_addresses`: the account addresses that the caller claims are
///   PDA signers.
///
/// # Errors
///
/// Returns `VmError::Syscall` if any seed set fails to derive or if the
/// derived address does not match the corresponding expected address.
pub fn validate_pda_signers(
    signer_seeds: &[&[&[u8]]],
    calling_program_id: &Hash,
    expected_addresses: &[Hash],
) -> Result<(), VmError> {
    if signer_seeds.len() != expected_addresses.len() {
        return Err(VmError::Syscall(format!(
            "PDA signer count mismatch: {} seed sets but {} expected addresses",
            signer_seeds.len(),
            expected_addresses.len(),
        )));
    }

    for (i, (seeds, expected)) in signer_seeds.iter().zip(expected_addresses).enumerate() {
        let derived = create_program_address(seeds, calling_program_id).map_err(|e| {
            VmError::Syscall(format!(
                "PDA signer validation failed for seed set {i}: {e}"
            ))
        })?;

        if derived != *expected {
            return Err(VmError::Syscall(format!(
                "PDA signer mismatch at index {i}: derived {} but expected {}",
                derived, expected,
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program_cache::ProgramCache;
    use nusantara_core::Account;
    use nusantara_crypto::hash;

    #[test]
    fn cpi_no_dispatch_fn() {
        let cache = ProgramCache::new(10);
        let program_id = hash(b"program_a");
        let target = hash(b"program_b");
        let mut accounts = vec![];
        let privileges: &[(bool, bool)] = &[];
        let mut host = VmHostState::new(
            &mut accounts,
            privileges,
            vec![],
            program_id,
            &cache,
            0,
            100_000,
        );

        let result = invoke_cross_program(&target, &[], &[], &mut host);
        assert!(result.is_err());
        match result.unwrap_err() {
            VmError::Syscall(msg) => assert!(msg.contains("no dispatch function")),
            other => panic!("expected Syscall error, got: {other}"),
        }
    }

    #[test]
    fn cpi_reentrancy_detected() {
        let cache = ProgramCache::new(10);
        let program_id = hash(b"program_a");
        let mut accounts = vec![];
        let privileges: &[(bool, bool)] = &[];
        let mut host = VmHostState::new(
            &mut accounts,
            privileges,
            vec![],
            program_id,
            &cache,
            0,
            100_000,
        );

        // program_id is already in call_stack (added by VmHostState::new)
        let result = invoke_cross_program(&program_id, &[], &[], &mut host);
        assert!(matches!(
            result.unwrap_err(),
            VmError::ReentrancyNotAllowed(_)
        ));
    }

    #[test]
    fn cpi_depth_exceeded() {
        let cache = ProgramCache::new(10);
        let program_id = hash(b"program_a");
        let target = hash(b"program_b");
        let mut accounts = vec![];
        let privileges: &[(bool, bool)] = &[];
        let mut host = VmHostState::new(
            &mut accounts,
            privileges,
            vec![],
            program_id,
            &cache,
            0,
            100_000,
        )
        .with_cpi_depth(crate::config::MAX_CPI_DEPTH);

        let result = invoke_cross_program(&target, &[], &[], &mut host);
        assert!(matches!(
            result.unwrap_err(),
            VmError::CpiDepthExceeded { .. }
        ));
    }

    #[test]
    fn cpi_account_index_out_of_bounds() {
        let cache = ProgramCache::new(10);
        let program_id = hash(b"program_a");
        let target = hash(b"program_b");
        let owner = hash(b"owner");
        let addr = hash(b"addr");
        let mut accounts = vec![(addr, Account::new(100, owner))];
        let privileges: &[(bool, bool)] = &[(true, true)];

        #[allow(clippy::too_many_arguments)]
        fn dummy_dispatch(
            _: &Hash,
            _: &[usize],
            _: &[u8],
            _: &mut [(Hash, Account)],
            _: &[(bool, bool)],
            _: &mut u64,
            _: u64,
            _: &ProgramCache,
            _: u32,
            _: &mut Vec<Hash>,
        ) -> Result<(), String> {
            Ok(())
        }

        let mut host = VmHostState::new(
            &mut accounts,
            privileges,
            vec![0],
            program_id,
            &cache,
            0,
            100_000,
        )
        .with_dispatch_fn(dummy_dispatch);

        // Index 5 is out of bounds for a 1-account list
        let result = invoke_cross_program(&target, &[5], &[], &mut host);
        assert!(matches!(result.unwrap_err(), VmError::AccountNotFound(5)));
    }

    #[test]
    fn pda_signer_valid() {
        let program_id = hash(b"my_program");
        let pda = create_program_address(&[b"account", &[255u8]], &program_id).unwrap();

        let result = validate_pda_signers(&[&[b"account", &[255u8]]], &program_id, &[pda]);
        assert!(result.is_ok());
    }

    #[test]
    fn pda_signer_mismatch() {
        let program_id = hash(b"my_program");
        let wrong_address = hash(b"not_a_pda");

        let result =
            validate_pda_signers(&[&[b"account", &[255u8]]], &program_id, &[wrong_address]);
        assert!(result.is_err());
        match result.unwrap_err() {
            VmError::Syscall(msg) => assert!(msg.contains("mismatch")),
            other => panic!("expected Syscall error, got: {other}"),
        }
    }

    #[test]
    fn pda_signer_wrong_program() {
        let program_a = hash(b"program_a");
        let program_b = hash(b"program_b");
        // Derive PDA from program_a
        let pda = create_program_address(&[b"seed"], &program_a).unwrap();

        // Try to validate as if program_b derived it
        let result = validate_pda_signers(&[&[b"seed"]], &program_b, &[pda]);
        assert!(result.is_err());
    }

    #[test]
    fn pda_signer_count_mismatch() {
        let program_id = hash(b"my_program");
        let pda = create_program_address(&[b"seed"], &program_id).unwrap();

        // 2 seed sets but only 1 expected address
        let result = validate_pda_signers(&[&[b"seed"], &[b"other"]], &program_id, &[pda]);
        assert!(result.is_err());
        match result.unwrap_err() {
            VmError::Syscall(msg) => assert!(msg.contains("count mismatch")),
            other => panic!("expected Syscall error, got: {other}"),
        }
    }

    #[test]
    fn pda_signer_invalid_seed() {
        let program_id = hash(b"my_program");
        let long_seed = [0u8; 33]; // exceeds MAX_SEED_LEN

        let result = validate_pda_signers(&[&[&long_seed]], &program_id, &[Hash::zero()]);
        assert!(result.is_err());
    }

    #[test]
    fn pda_signer_empty() {
        let program_id = hash(b"my_program");
        let result = validate_pda_signers(&[], &program_id, &[]);
        assert!(result.is_ok(), "empty signer set should succeed");
    }

    #[test]
    fn pda_signer_multiple_valid() {
        let program_id = hash(b"my_program");
        let pda1 = create_program_address(&[b"acct_1"], &program_id).unwrap();
        let pda2 = create_program_address(&[b"acct_2"], &program_id).unwrap();

        let result =
            validate_pda_signers(&[&[b"acct_1"], &[b"acct_2"]], &program_id, &[pda1, pda2]);
        assert!(result.is_ok());
    }

    #[test]
    fn cpi_insufficient_compute() {
        let cache = ProgramCache::new(10);
        let program_id = hash(b"program_a");
        let target = hash(b"program_b");
        let mut accounts = vec![];
        let privileges: &[(bool, bool)] = &[];
        let mut host = VmHostState::new(
            &mut accounts,
            privileges,
            vec![],
            program_id,
            &cache,
            0,
            COST_CPI_BASE - 1, // not enough for the base cost
        );

        let result = invoke_cross_program(&target, &[], &[], &mut host);
        assert!(matches!(result.unwrap_err(), VmError::ComputeExceeded));
    }
}
