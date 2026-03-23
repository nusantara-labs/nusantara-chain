//! Host state passed to WASM syscalls during execution.
//!
//! [`VmHostState`] is the mutable context that WASM programs interact with
//! through the syscall interface. It holds references to the transaction's
//! accounts, privilege metadata, the program cache, and bookkeeping for CPI
//! depth, reentrancy detection, return data, and the bump-allocator heap
//! pointer.
//!
//! ## CPI dispatch
//!
//! The vm crate cannot depend on `nusantara-runtime` (that would be circular).
//! Instead, runtime supplies a [`DispatchFn`] function pointer that the VM
//! calls when a WASM program issues a cross-program invocation.

use nusantara_core::Account;
use nusantara_crypto::Hash;

use crate::config::{MAX_CPI_DEPTH, MAX_RETURN_DATA_SIZE};
use crate::error::VmError;
use crate::program_cache::ProgramCache;

/// Function-pointer type for CPI dispatch back into the runtime.
///
/// This breaks the circular dependency between the `vm` and `runtime` crates:
/// the runtime registers a concrete dispatch implementation when it creates
/// a [`VmHostState`], and the VM calls it when a WASM program invokes
/// `nusa_invoke`.
///
/// # Parameters
///
/// | name               | description |
/// |--------------------|-------------|
/// | `program_id`       | The program to invoke |
/// | `account_indices`  | Which accounts to pass (indices into the account list) |
/// | `instruction_data` | The instruction data |
/// | `accounts`         | Mutable reference to the account list |
/// | `account_privileges` | `(is_signer, is_writable)` for each account |
/// | `compute_remaining` | Mutable remaining compute units |
/// | `slot`             | Current slot |
/// | `program_cache`    | The program cache |
/// | `cpi_depth`        | Current CPI nesting depth |
/// | `call_stack`       | Call stack for reentrancy detection |
///
/// Returns `Ok(())` on success or an error description string.
pub type DispatchFn = fn(
    program_id: &Hash,
    account_indices: &[usize],
    instruction_data: &[u8],
    accounts: &mut [(Hash, Account)],
    account_privileges: &[(bool, bool)],
    compute_remaining: &mut u64,
    slot: u64,
    program_cache: &ProgramCache,
    cpi_depth: u32,
    call_stack: &mut Vec<Hash>,
) -> Result<(), String>;

/// Mutable state accessible to WASM syscalls during a single program execution.
pub struct VmHostState<'a> {
    /// Program accounts: `(address, account)` pairs accessible to this invocation.
    pub accounts: &'a mut [(Hash, Account)],
    /// Privilege flags: `(is_signer, is_writable)` for each account.
    pub account_privileges: &'a [(bool, bool)],
    /// Index mapping: WASM account index -> outer account-list index.
    pub account_indices: Vec<usize>,
    /// The program being executed.
    pub program_id: Hash,
    /// Current CPI nesting depth (0 = top-level invocation).
    pub cpi_depth: u32,
    /// Call stack for reentrancy detection.
    pub call_stack: Vec<Hash>,
    /// Return data from this or an inner invocation.
    pub return_data: Option<(Hash, Vec<u8>)>,
    /// Bump-allocator offset in the WASM linear memory.
    pub heap_offset: u32,
    /// Program cache for CPI targets.
    pub program_cache: &'a ProgramCache,
    /// CPI dispatch function supplied by the runtime.
    pub dispatch_fn: Option<DispatchFn>,
    /// Current slot number.
    pub slot: u64,
    /// Remaining compute units (mirrored to/from wasmi fuel).
    pub compute_remaining: u64,
    /// Log messages emitted by the program.
    pub log_messages: Vec<String>,
}

impl<'a> VmHostState<'a> {
    /// Create a new host state for a top-level program invocation.
    pub fn new(
        accounts: &'a mut [(Hash, Account)],
        account_privileges: &'a [(bool, bool)],
        account_indices: Vec<usize>,
        program_id: Hash,
        program_cache: &'a ProgramCache,
        slot: u64,
        compute_remaining: u64,
    ) -> Self {
        Self {
            accounts,
            account_privileges,
            account_indices,
            program_id,
            cpi_depth: 0,
            call_stack: vec![program_id],
            return_data: None,
            heap_offset: 0,
            program_cache,
            dispatch_fn: None,
            slot,
            compute_remaining,
            log_messages: Vec::new(),
        }
    }

    /// Attach a CPI dispatch function.
    pub fn with_dispatch_fn(mut self, dispatch_fn: DispatchFn) -> Self {
        self.dispatch_fn = Some(dispatch_fn);
        self
    }

    /// Set the initial CPI depth (for nested invocations).
    pub fn with_cpi_depth(mut self, depth: u32) -> Self {
        self.cpi_depth = depth;
        self
    }

    /// Set the initial call stack (for nested invocations).
    pub fn with_call_stack(mut self, call_stack: Vec<Hash>) -> Self {
        self.call_stack = call_stack;
        self
    }

    /// Deduct `units` from the remaining compute budget.
    ///
    /// Returns [`VmError::ComputeExceeded`] if there are not enough units
    /// remaining, in which case the budget is set to zero.
    pub fn consume_compute(&mut self, units: u64) -> Result<(), VmError> {
        if units > self.compute_remaining {
            self.compute_remaining = 0;
            return Err(VmError::ComputeExceeded);
        }
        self.compute_remaining -= units;
        Ok(())
    }

    /// Store return data for this invocation.
    ///
    /// Returns [`VmError::ReturnDataTooLarge`] if the data exceeds the limit.
    pub fn set_return_data(&mut self, program_id: Hash, data: Vec<u8>) -> Result<(), VmError> {
        if data.len() > MAX_RETURN_DATA_SIZE {
            return Err(VmError::ReturnDataTooLarge {
                size: data.len(),
                max: MAX_RETURN_DATA_SIZE,
            });
        }
        self.return_data = Some((program_id, data));
        Ok(())
    }

    /// Verify that the CPI depth limit has not been reached.
    pub fn check_cpi_depth(&self) -> Result<(), VmError> {
        if self.cpi_depth >= MAX_CPI_DEPTH {
            return Err(VmError::CpiDepthExceeded {
                depth: self.cpi_depth,
                max: MAX_CPI_DEPTH,
            });
        }
        Ok(())
    }

    /// Verify that invoking `program_id` would not cause reentrancy.
    pub fn check_reentrancy(&self, program_id: &Hash) -> Result<(), VmError> {
        if self.call_stack.contains(program_id) {
            return Err(VmError::ReentrancyNotAllowed(format!(
                "program {} already in call stack",
                program_id
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    fn make_cache() -> ProgramCache {
        ProgramCache::new(10)
    }

    #[test]
    fn consume_compute_success() {
        let cache = make_cache();
        let pid = hash(b"prog");
        let mut accounts = [];
        let privs: &[(bool, bool)] = &[];
        let mut state = VmHostState::new(&mut accounts, privs, vec![], pid, &cache, 0, 1000);

        state.consume_compute(500).unwrap();
        assert_eq!(state.compute_remaining, 500);

        state.consume_compute(500).unwrap();
        assert_eq!(state.compute_remaining, 0);
    }

    #[test]
    fn consume_compute_exceeded() {
        let cache = make_cache();
        let pid = hash(b"prog");
        let mut accounts = [];
        let privs: &[(bool, bool)] = &[];
        let mut state = VmHostState::new(&mut accounts, privs, vec![], pid, &cache, 0, 100);

        let err = state.consume_compute(101).unwrap_err();
        assert!(matches!(err, VmError::ComputeExceeded));
        assert_eq!(state.compute_remaining, 0);
    }

    #[test]
    fn set_return_data_ok() {
        let cache = make_cache();
        let pid = hash(b"prog");
        let mut accounts = [];
        let privs: &[(bool, bool)] = &[];
        let mut state = VmHostState::new(&mut accounts, privs, vec![], pid, &cache, 0, 1000);

        state.set_return_data(pid, vec![1, 2, 3]).unwrap();
        let (id, data) = state.return_data.as_ref().unwrap();
        assert_eq!(*id, pid);
        assert_eq!(data, &[1, 2, 3]);
    }

    #[test]
    fn set_return_data_too_large() {
        let cache = make_cache();
        let pid = hash(b"prog");
        let mut accounts = [];
        let privs: &[(bool, bool)] = &[];
        let mut state = VmHostState::new(&mut accounts, privs, vec![], pid, &cache, 0, 1000);

        let big = vec![0u8; MAX_RETURN_DATA_SIZE + 1];
        let err = state.set_return_data(pid, big).unwrap_err();
        assert!(matches!(err, VmError::ReturnDataTooLarge { .. }));
    }

    #[test]
    fn cpi_depth_check() {
        let cache = make_cache();
        let pid = hash(b"prog");
        let mut accounts = [];
        let privs: &[(bool, bool)] = &[];
        let mut state = VmHostState::new(&mut accounts, privs, vec![], pid, &cache, 0, 1000);

        // Depth 0 should be fine
        state.check_cpi_depth().unwrap();

        // At max depth should fail
        state.cpi_depth = MAX_CPI_DEPTH;
        let err = state.check_cpi_depth().unwrap_err();
        assert!(matches!(err, VmError::CpiDepthExceeded { .. }));
    }

    #[test]
    fn reentrancy_detection() {
        let cache = make_cache();
        let pid = hash(b"prog_a");
        let other = hash(b"prog_b");
        let mut accounts = [];
        let privs: &[(bool, bool)] = &[];
        let state = VmHostState::new(&mut accounts, privs, vec![], pid, &cache, 0, 1000);

        // prog_a is already on the call stack
        let err = state.check_reentrancy(&pid).unwrap_err();
        assert!(matches!(err, VmError::ReentrancyNotAllowed(_)));

        // prog_b is not on the call stack
        state.check_reentrancy(&other).unwrap();
    }

    #[test]
    fn builder_methods() {
        let cache = make_cache();
        let pid = hash(b"prog");
        let mut accounts = [];
        let privs: &[(bool, bool)] = &[];

        #[allow(clippy::too_many_arguments)]
        fn noop_dispatch(
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

        let state = VmHostState::new(&mut accounts, privs, vec![], pid, &cache, 42, 5000)
            .with_dispatch_fn(noop_dispatch)
            .with_cpi_depth(2)
            .with_call_stack(vec![pid]);

        assert!(state.dispatch_fn.is_some());
        assert_eq!(state.cpi_depth, 2);
        assert_eq!(state.call_stack, vec![pid]);
        assert_eq!(state.slot, 42);
    }
}
