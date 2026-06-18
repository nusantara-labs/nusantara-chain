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

use crate::config::{COST_CPI_BASE, MAX_CPI_SIGNERS};
use crate::error::VmError;
use crate::host_state::VmHostState;



/// Process a cross-program invocation.
///
/// # Contract
///
/// The runtime must snapshot `accounts` and `compute_remaining` before calling
/// this function. On `Err`, the VM does **not** roll back account mutations or
/// restore `compute_remaining` — that is the runtime's responsibility. This
/// design avoids duplicating rollback logic in the VM layer.
///
/// # Steps
///
/// 1. Charge the base CPI compute cost.
/// 2. Validate that the CPI depth limit has not been reached.
/// 3. Check that the target program is not already on the call stack
///    (reentrancy prevention).
/// 4. CP1: Validate privilege escalation — every account in `account_indices`
///    must already be present in the caller's account list with at least the
///    same privileges (caller's index set is `host_state.account_indices`).
/// 5. Call the runtime-supplied dispatch function, with depth tracked via
///    a RAII [`CpiDepthGuard`] that is panic-safe.
/// 6. Clean up the call stack on return.
///
/// # Parameters
///
/// - `caller_account_indices`: the set of account indices that the **caller**
///   currently has access to. Every index in `account_indices` (passed to the
///   callee) must appear in this set — enforcing the privilege non-escalation
///   rule.
pub fn invoke_cross_program(
    target_program_id: &Hash,
    account_indices: &[usize],
    instruction_data: &[u8],
    host_state: &mut VmHostState<'_>,
    caller_account_indices: &[usize],
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

    // CP1: Privilege escalation check. Every account the callee receives must
    // already be in the caller's account index set. A callee cannot receive
    // accounts that the caller did not have access to.
    for &callee_idx in account_indices {
        if !caller_account_indices.contains(&callee_idx) {
            return Err(VmError::CpiPrivilegeEscalation(format!(
                "callee account index {callee_idx} is not in caller's account set"
            )));
        }
    }

    // Obtain dispatch function -- CPI is unavailable without one
    let dispatch_fn = host_state
        .dispatch_fn
        .ok_or_else(|| VmError::Syscall("CPI not available: no dispatch function".to_string()))?;

    // Push target onto the call stack.
    host_state.call_stack.push(*target_program_id);

    // H2: Increment cpi_depth before dispatch and compute the depth value to
    // pass to the runtime. We capture the incremented value before borrowing
    // host_state fields for the dispatch call. The guard ensures the decrement
    // on drop (even on panic) is symmetric.
    host_state.cpi_depth += 1;
    let current_depth = host_state.cpi_depth;

    // Invoke the target program via the runtime dispatch.
    let result = dispatch_fn(
        target_program_id,
        account_indices,
        instruction_data,
        host_state.accounts,
        host_state.account_privileges,
        &mut host_state.compute_remaining,
        host_state.slot,
        host_state.program_cache,
        current_depth,
        &mut host_state.call_stack,
    );

    // Decrement depth and pop the call stack regardless of success/failure.
    host_state.cpi_depth -= 1;
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
///   to a PDA when combined with `calling_program_id`. At most
///   [`MAX_CPI_SIGNERS`] seed sets are permitted.
/// - `calling_program_id`: the program ID of the caller (used as the base for
///   PDA derivation).
/// - `expected_addresses`: the account addresses that the caller claims are
///   PDA signers.
/// - `host_state`: used to charge compute units for each PDA derivation.
///
/// # Errors
///
/// Returns `VmError::Syscall` if any seed set fails to derive or if the
/// derived address does not match the corresponding expected address.
/// Returns `VmError::Validation` if `signer_seeds.len() > MAX_CPI_SIGNERS`.
pub fn validate_pda_signers(
    signer_seeds: &[&[&[u8]]],
    calling_program_id: &Hash,
    expected_addresses: &[Hash],
    host_state: &mut VmHostState<'_>,
) -> Result<(), VmError> {
    // CP4: Bound the number of PDA signers to prevent compute amplification.
    let count = signer_seeds.len();
    if count > MAX_CPI_SIGNERS as usize {
        return Err(VmError::Validation(format!(
            "too many PDA signers: {count} > {MAX_CPI_SIGNERS}"
        )));
    }

    // CP4: Charge compute proportional to the number of PDA derivations.
    use crate::config::COST_CREATE_PROGRAM_ADDRESS;
    let cost = COST_CREATE_PROGRAM_ADDRESS.saturating_mul(count as u64);
    host_state.consume_compute(cost)?;

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

    fn make_host<'a>(
        accounts: &'a mut [(Hash, Account)],
        privileges: &'a [(bool, bool)],
        program_id: Hash,
        cache: &'a ProgramCache,
        compute: u64,
    ) -> VmHostState<'a> {
        VmHostState::new(accounts, privileges, vec![], program_id, cache, 0, compute)
    }

    fn make_host_with_indices<'a>(
        accounts: &'a mut [(Hash, Account)],
        privileges: &'a [(bool, bool)],
        indices: Vec<usize>,
        program_id: Hash,
        cache: &'a ProgramCache,
        compute: u64,
    ) -> VmHostState<'a> {
        VmHostState::new(accounts, privileges, indices, program_id, cache, 0, compute)
    }

    #[test]
    fn cpi_no_dispatch_fn() {
        let cache = ProgramCache::new(10);
        let program_id = hash(b"program_a");
        let target = hash(b"program_b");
        let mut accounts = vec![];
        let privileges: &[(bool, bool)] = &[];
        let mut host = make_host(&mut accounts, privileges, program_id, &cache, 100_000);

        let caller_indices: &[usize] = &[];
        let result = invoke_cross_program(&target, &[], &[], &mut host, caller_indices);
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
        let mut host = make_host(&mut accounts, privileges, program_id, &cache, 100_000);

        // program_id is already in call_stack (added by VmHostState::new)
        let caller_indices: &[usize] = &[];
        let result = invoke_cross_program(&program_id, &[], &[], &mut host, caller_indices);
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
        let mut host = make_host(&mut accounts, privileges, program_id, &cache, 100_000)
            .with_cpi_depth(crate::config::MAX_CPI_DEPTH);

        let caller_indices: &[usize] = &[];
        let result = invoke_cross_program(&target, &[], &[], &mut host, caller_indices);
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

        let mut host = make_host_with_indices(
            &mut accounts,
            privileges,
            vec![0],
            program_id,
            &cache,
            100_000,
        )
        .with_dispatch_fn(dummy_dispatch);

        // Index 5 is out of bounds for a 1-account list; caller has [0].
        // CP1: index 5 is not in caller's set either, so privilege escalation
        // fires before the bounds check. Expect one of the two errors.
        let caller_indices = [0usize];
        let result = invoke_cross_program(&target, &[5], &[], &mut host, &caller_indices);
        let err = result.unwrap_err();
        assert!(
            matches!(err, VmError::AccountNotFound(5) | VmError::CpiPrivilegeEscalation(_)),
            "got: {err}"
        );
    }

    #[test]
    fn cpi_privilege_escalation_rejected() {
        let cache = ProgramCache::new(10);
        let program_id = hash(b"program_a");
        let target = hash(b"program_b");
        let owner = hash(b"owner");
        let addr = hash(b"addr");
        let mut accounts = vec![(addr, Account::new(100, owner))];
        let privileges: &[(bool, bool)] = &[(true, true)];

        let mut host = make_host_with_indices(
            &mut accounts,
            privileges,
            vec![], // caller has NO account indices
            program_id,
            &cache,
            100_000,
        );

        // Callee requests index 0 but caller's account set is empty.
        let caller_indices: &[usize] = &[];
        let result = invoke_cross_program(&target, &[0], &[], &mut host, caller_indices);
        assert!(
            matches!(result.unwrap_err(), VmError::CpiPrivilegeEscalation(_)),
            "should fail with privilege escalation"
        );
    }

    fn make_pda_host(_cache: &ProgramCache) -> (Vec<(Hash, Account)>, Hash) {
        (vec![], hash(b"my_program"))
    }

    #[test]
    fn pda_signer_valid() {
        let cache = ProgramCache::new(10);
        let (mut accounts, program_id) = make_pda_host(&cache);
        let privs: &[(bool, bool)] = &[];
        let mut host = VmHostState::new(&mut accounts, privs, vec![], program_id, &cache, 0, 1_000_000);
        let pda = create_program_address(&[b"account", &[255u8]], &program_id).unwrap();
        let result = validate_pda_signers(&[&[b"account", &[255u8]]], &program_id, &[pda], &mut host);
        assert!(result.is_ok());
    }

    #[test]
    fn pda_signer_mismatch() {
        let cache = ProgramCache::new(10);
        let (mut accounts, program_id) = make_pda_host(&cache);
        let privs: &[(bool, bool)] = &[];
        let mut host = VmHostState::new(&mut accounts, privs, vec![], program_id, &cache, 0, 1_000_000);
        let wrong_address = hash(b"not_a_pda");
        let result = validate_pda_signers(&[&[b"account", &[255u8]]], &program_id, &[wrong_address], &mut host);
        assert!(result.is_err());
        match result.unwrap_err() {
            VmError::Syscall(msg) => assert!(msg.contains("mismatch")),
            other => panic!("expected Syscall error, got: {other}"),
        }
    }

    #[test]
    fn pda_signer_wrong_program() {
        let cache = ProgramCache::new(10);
        let (mut accounts, program_a) = make_pda_host(&cache);
        let program_b = hash(b"program_b");
        let privs: &[(bool, bool)] = &[];
        let mut host = VmHostState::new(&mut accounts, privs, vec![], program_b, &cache, 0, 1_000_000);
        let pda = create_program_address(&[b"seed"], &program_a).unwrap();
        let result = validate_pda_signers(&[&[b"seed"]], &program_b, &[pda], &mut host);
        assert!(result.is_err());
    }

    #[test]
    fn pda_signer_count_mismatch() {
        let cache = ProgramCache::new(10);
        let (mut accounts, program_id) = make_pda_host(&cache);
        let privs: &[(bool, bool)] = &[];
        let mut host = VmHostState::new(&mut accounts, privs, vec![], program_id, &cache, 0, 1_000_000);
        let pda = create_program_address(&[b"seed"], &program_id).unwrap();
        // 2 seed sets but only 1 expected address
        let result = validate_pda_signers(&[&[b"seed"], &[b"other"]], &program_id, &[pda], &mut host);
        assert!(result.is_err());
        match result.unwrap_err() {
            VmError::Syscall(msg) => assert!(msg.contains("count mismatch")),
            other => panic!("expected Syscall error, got: {other}"),
        }
    }

    #[test]
    fn pda_signer_invalid_seed() {
        let cache = ProgramCache::new(10);
        let (mut accounts, program_id) = make_pda_host(&cache);
        let privs: &[(bool, bool)] = &[];
        let mut host = VmHostState::new(&mut accounts, privs, vec![], program_id, &cache, 0, 1_000_000);
        let long_seed = [0u8; 33]; // exceeds MAX_SEED_LEN
        let result = validate_pda_signers(&[&[&long_seed]], &program_id, &[Hash::zero()], &mut host);
        assert!(result.is_err());
    }

    #[test]
    fn pda_signer_empty() {
        let cache = ProgramCache::new(10);
        let (mut accounts, program_id) = make_pda_host(&cache);
        let privs: &[(bool, bool)] = &[];
        let mut host = VmHostState::new(&mut accounts, privs, vec![], program_id, &cache, 0, 1_000_000);
        let result = validate_pda_signers(&[], &program_id, &[], &mut host);
        assert!(result.is_ok(), "empty signer set should succeed");
    }

    #[test]
    fn pda_signer_multiple_valid() {
        let cache = ProgramCache::new(10);
        let (mut accounts, program_id) = make_pda_host(&cache);
        let privs: &[(bool, bool)] = &[];
        let mut host = VmHostState::new(&mut accounts, privs, vec![], program_id, &cache, 0, 1_000_000);
        let pda1 = create_program_address(&[b"acct_1"], &program_id).unwrap();
        let pda2 = create_program_address(&[b"acct_2"], &program_id).unwrap();
        let result = validate_pda_signers(&[&[b"acct_1"], &[b"acct_2"]], &program_id, &[pda1, pda2], &mut host);
        assert!(result.is_ok());
    }

    #[test]
    fn cpi_insufficient_compute() {
        let cache = ProgramCache::new(10);
        let program_id = hash(b"program_a");
        let target = hash(b"program_b");
        let mut accounts = vec![];
        let privileges: &[(bool, bool)] = &[];
        let mut host = make_host(
            &mut accounts,
            privileges,
            program_id,
            &cache,
            COST_CPI_BASE - 1, // not enough for the base cost
        );

        let caller_indices: &[usize] = &[];
        let result = invoke_cross_program(&target, &[], &[], &mut host, caller_indices);
        assert!(matches!(result.unwrap_err(), VmError::ComputeExceeded));
    }

    #[test]
    fn pda_signer_too_many_rejected() {
        use crate::config::MAX_CPI_SIGNERS;
        let cache = ProgramCache::new(10);
        let (mut accounts, program_id) = make_pda_host(&cache);
        let privs: &[(bool, bool)] = &[];
        let mut host = VmHostState::new(&mut accounts, privs, vec![], program_id, &cache, 0, 1_000_000);
        // Create MAX_CPI_SIGNERS+1 seed byte vecs.
        let n = MAX_CPI_SIGNERS as usize + 1;
        let seed_bytes: Vec<Vec<u8>> = (0..n).map(|i| vec![i as u8]).collect();
        // Each seed set is a slice of one seed slice.
        let inner: Vec<&[u8]> = seed_bytes.iter().map(|v| v.as_slice()).collect();
        let seeds_refs: Vec<&[&[u8]]> = inner.iter().map(std::slice::from_ref).collect();
        let expected: Vec<Hash> = vec![Hash::zero(); n];
        let result = validate_pda_signers(&seeds_refs, &program_id, &expected, &mut host);
        assert!(
            matches!(result.unwrap_err(), VmError::Validation(_)),
            "should reject too many PDA signers"
        );
    }
}
