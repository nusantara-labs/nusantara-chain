//! WASM program executor.
//!
//! [`WasmExecutor`] is the main entry point for running WASM smart contracts
//! on the Nusantara blockchain. It performs the following steps:
//!
//! 1. **Compile** the bytecode (or fetch from the program cache).
//! 2. **Charge** instantiation compute cost.
//! 3. **Instantiate** the module with registered syscalls and fuel metering.
//! 4. **Serialize** the instruction data and program ID into WASM linear memory.
//! 5. **Call** the `entrypoint(num_accounts, data_ptr, data_len, program_id_ptr) -> i64`.
//! 6. **Sync** the fuel consumption back to the host state's compute meter.
//!
//! The executor uses wasmi's fuel metering to enforce compute-unit limits: each
//! wasmi instruction consumes one unit of fuel, and the initial fuel is set to
//! the remaining compute budget of the transaction.
//!
//! ## Engine sharing
//!
//! The wasmi [`Engine`] is owned by [`ProgramCache`] and created once at startup.
//! All compiled modules use this shared engine instance. This avoids the cost
//! of creating a new engine per execution and ensures cached modules are
//! compatible with the stores that execute them.
//!
//! ## Bytecode-hash keying
//!
//! The program cache is keyed by the SHA3-512 hash of the bytecode, not the
//! program's on-chain address. This provides automatic cache invalidation when
//! a program is upgraded (new bytecode = new hash) and deduplication when
//! multiple addresses deploy identical bytecode.

use nusantara_crypto::{Hash, hash as crypto_hash};
use tracing::instrument;
use wasmi::{Linker, Module, Store, TrapCode};

use crate::config::COST_INSTANTIATION;
use crate::error::VmError;
use crate::host_state::VmHostState;
use crate::program_cache::ProgramCache;
use crate::syscall;
use crate::syscall::memory::{HEAP_SIZE, HEAP_START};

/// Stateless WASM executor.
///
/// All mutable state lives in [`VmHostState`]; the executor itself carries no
/// fields. This design allows the same executor logic to be called from
/// multiple contexts (top-level execution, CPI) without shared mutable state.
pub struct WasmExecutor;

impl WasmExecutor {
    /// Execute a WASM program.
    ///
    /// # Parameters
    ///
    /// - `bytecode`         -- raw WASM bytes of the program
    /// - `program_id`       -- the program account's address hash
    /// - `instruction_data` -- data payload passed to the program
    /// - `host_state`       -- mutable context with accounts, privileges, etc.
    /// - `program_cache`    -- LRU cache for compiled modules
    ///
    /// # Returns
    ///
    /// `Ok(())` on success. A non-zero return value from the entrypoint is
    /// reported as [`VmError::ProgramError`].
    #[instrument(skip_all, fields(program = %program_id))]
    pub fn execute(
        bytecode: &[u8],
        program_id: &Hash,
        instruction_data: &[u8],
        host_state: &mut VmHostState<'_>,
        program_cache: &ProgramCache,
    ) -> Result<(), VmError> {
        // 1. Use the shared engine from the program cache. The engine is created
        //    once at startup with fuel metering enabled and floats disabled.
        let engine = program_cache.engine();

        // 2. Compute the bytecode hash and use it as the cache key. This ensures
        //    that upgraded programs (new bytecode at the same address) always get
        //    recompiled, and identical bytecodes share the same cached module.
        let bytecode_hash = crypto_hash(bytecode);

        // E6+E7: Charge instantiation cost BEFORE compilation so that a
        // malicious program cannot trigger an expensive compile without burning
        // compute. On cache hits the charge is the same (slight overcharge is
        // acceptable; the alternative of splitting costs would require two config
        // constants for a marginal gain).
        host_state.consume_compute(COST_INSTANTIATION)?;

        let module = if let Some(cached) = program_cache.get(&bytecode_hash) {
            metrics::counter!("nusantara_vm_cache_hits").increment(1);
            // Cached modules are guaranteed start-section-free by construction:
            // the module was validated (including has_start_section) before being
            // inserted into the cache on the first cache-miss path below.
            cached
        } else {
            metrics::counter!("nusantara_vm_cache_misses").increment(1);

            // E2: has_start_section is checked only on cache miss, before
            // inserting into the cache. This avoids the byte scan overhead on
            // every cache-hit invocation of the same program.
            if crate::validate::has_start_section(bytecode) {
                return Err(VmError::HasStartFunction);
            }

            let module =
                Module::new(engine, bytecode).map_err(|e| VmError::Compilation(e.to_string()))?;
            program_cache.insert(bytecode_hash, module.clone());
            module
        };

        // 3. Create store and seed it with the remaining compute budget as fuel.
        let fuel = host_state.compute_remaining;
        let mut store: Store<()> = Store::new(engine, ());
        store
            .set_fuel(fuel)
            .map_err(|e| VmError::Trap(e.to_string()))?;

        // 4. Register syscalls in the linker.
        //
        // TODO(M1): The store is currently `Store<()>` because `VmHostState<'a>`
        // borrows `accounts: &'a mut [(Hash, Account)]` with a non-'static lifetime,
        // and wasmi's `Caller<T>` requires `T: 'static` for host-function access.
        // Migrating to `Store<VmHostState>` would require either:
        //   (a) Erasing the lifetime via a raw pointer (unsafe, very careful),
        //   (b) Restructuring VmHostState to use Arc<Mutex<...>> for accounts, or
        //   (c) Using a thread-local for the in-flight host state pointer.
        // Until that migration is done, `nusa_alloc` returns 0 (OOM) from the stub
        // in syscall/memory.rs, and `nusa_log` is a no-op stub. Programs must not
        // depend on these syscalls returning valid values in the current VM version.
        let mut linker: Linker<()> = Linker::new(engine);
        syscall::link_all(&mut linker, engine)?;

        // 5. Instantiate. No start function is present (checked on cache-miss
        //    path above; cached modules are guaranteed start-section-free).
        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .map_err(|e| VmError::Instantiation(e.to_string()))?;

        // 6. Obtain the exported memory.
        let memory = instance
            .get_memory(&store, "memory")
            .ok_or(VmError::MissingMemory)?;

        // 7. Initialize heap_offset to HEAP_START if not already set.
        //    E4: heap_offset=0 means data would be written into [0..], which
        //    overlaps the WASM stack region ([0, HEAP_START)). We initialize it
        //    to HEAP_START so all allocations live in the designated heap region.
        if host_state.heap_offset == 0 {
            host_state.heap_offset = HEAP_START;
        }

        // 8. Write instruction data into WASM linear memory.
        let data_offset: u32 = host_state.heap_offset;
        // E1: use try_into() to catch truncation instead of silent `as u32`.
        let data_len: u32 = instruction_data
            .len()
            .try_into()
            .map_err(|_| VmError::Validation("instruction_data too large".into()))?;

        // E5: Pre-check that the memory region is large enough to hold both
        // the instruction data and the program_id (64 bytes) that follows.
        let required_end: u64 = data_offset as u64 + data_len as u64 + 64; // +64 for program_id
        let mem_bytes: u64 = memory.size(&store) as u64 * 65_536;
        if mem_bytes < required_end {
            return Err(VmError::MemoryOutOfBounds {
                offset: data_offset,
                len: data_len.saturating_add(64),
            });
        }
        // Also enforce the heap-region cap: direct memory.write must not push
        // past HEAP_START + HEAP_SIZE, the same bound heap_alloc enforces.
        let heap_end: u64 = HEAP_START as u64 + HEAP_SIZE as u64;
        if required_end > heap_end {
            return Err(VmError::HeapExhausted {
                need: data_len.saturating_add(64),
                available: heap_end.saturating_sub(data_offset as u64) as u32,
            });
        }

        if !instruction_data.is_empty() {
            memory
                .write(&mut store, data_offset as usize, instruction_data)
                .map_err(|_| VmError::MemoryOutOfBounds {
                    offset: data_offset,
                    len: data_len,
                })?;
        }

        // E4: Use checked_add to catch heap overflow instead of silent wrapping.
        host_state.heap_offset = host_state
            .heap_offset
            .checked_add(data_len)
            .ok_or(VmError::HeapExhausted {
                need: data_len,
                available: 0,
            })?;

        // 9. Write program ID (64 bytes) into WASM linear memory.
        // E1: account_indices.len() -> try_into() to prevent truncation.
        let num_accounts: i32 = host_state
            .account_indices
            .len()
            .try_into()
            .map_err(|_| VmError::Validation("too many accounts".into()))?;

        let program_id_offset: u32 = host_state.heap_offset;
        memory
            .write(
                &mut store,
                program_id_offset as usize,
                program_id.as_bytes(),
            )
            .map_err(|_| VmError::MemoryOutOfBounds {
                offset: program_id_offset,
                len: 64,
            })?;

        // E4: checked_add for program_id write (64 bytes fixed size).
        host_state.heap_offset = host_state
            .heap_offset
            .checked_add(64)
            .ok_or(VmError::HeapExhausted {
                need: 64,
                available: 0,
            })?;

        // 10. Resolve the entrypoint and call it.
        let entrypoint = instance
            .get_typed_func::<(i32, i32, i32, i32), i64>(&store, "entrypoint")
            .map_err(|_| VmError::MissingEntrypoint)?;

        // E1: data_offset and data_len must fit in i32 for the entrypoint ABI.
        let data_offset_i32: i32 = data_offset
            .try_into()
            .map_err(|_| VmError::Validation("data_offset too large for i32".into()))?;
        let data_len_i32: i32 = data_len
            .try_into()
            .map_err(|_| VmError::Validation("data_len too large for i32".into()))?;
        let program_id_offset_i32: i32 = program_id_offset
            .try_into()
            .map_err(|_| VmError::Validation("program_id_offset too large for i32".into()))?;

        let result = entrypoint
            .call(
                &mut store,
                (
                    num_accounts,
                    data_offset_i32,
                    data_len_i32,
                    program_id_offset_i32,
                ),
            )
            .map_err(|e| {
                // E3: Use as_trap_code() to detect fuel exhaustion precisely,
                // rather than the heuristic `get_fuel() == 0`. The wasmi 1.1
                // API exposes `Error::as_trap_code()` which maps fuel-out
                // errors to `TrapCode::OutOfFuel`.
                if e.as_trap_code() == Some(TrapCode::OutOfFuel) {
                    VmError::ComputeExceeded
                } else {
                    VmError::Trap(e.to_string())
                }
            })?;

        // 11. Sync fuel consumption back to the host state's compute meter.
        // Propagate fuel-read errors instead of silently zeroing remaining fuel,
        // which would over-charge the caller's compute budget.
        let remaining_fuel = store
            .get_fuel()
            .map_err(|e| VmError::Trap(format!("fuel read: {e}")))?;
        let fuel_consumed = fuel.saturating_sub(remaining_fuel);
        host_state.compute_remaining = remaining_fuel;

        metrics::counter!("nusantara_vm_executions").increment(1);
        metrics::counter!("nusantara_vm_compute_consumed").increment(fuel_consumed);

        // E8: Non-zero entrypoint result is a program error. Return type is
        // now `Result<(), VmError>` -- callers no longer receive the raw i64.
        if result != 0 {
            return Err(VmError::ProgramError(result));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    /// Minimal WASM module (WAT text) that exports a memory and an entrypoint
    /// function with the expected signature `(i32, i32, i32, i32) -> i64`.
    /// The entrypoint simply returns 0 (success).
    ///
    /// Memory is declared with 48 initial pages (3 MiB) so that the heap
    /// region starting at HEAP_START (0x300000 = 3 MiB) is accessible. An
    /// explicit maximum of 64 pages is required by V2 validation.
    const MINIMAL_WAT: &str = r#"
        (module
            (memory (export "memory") 49 64)
            (func (export "entrypoint") (param i32 i32 i32 i32) (result i64)
                i64.const 0
            )
        )
    "#;

    /// A second WASM module with different bytecode that returns 0 (success).
    /// The difference is an extra unreachable no-op function to ensure the
    /// bytecode hash differs from `MINIMAL_WAT`.
    const DIFFERENT_WAT: &str = r#"
        (module
            (memory (export "memory") 49 64)
            (func (export "entrypoint") (param i32 i32 i32 i32) (result i64)
                i64.const 0
            )
            (func $unused (result i32)
                i32.const 42
            )
        )
    "#;

    /// Compile WAT text to WASM binary bytes.
    fn wat_to_wasm(wat: &str) -> Vec<u8> {
        wat::parse_str(wat).expect("WAT should be valid")
    }

    #[test]
    fn invalid_bytecode_fails_compilation() {
        let cache = ProgramCache::new(10);
        let program_id = hash(b"test_program");
        let mut accounts = vec![];
        let privileges: &[(bool, bool)] = &[];
        let mut host_state = VmHostState::new(
            &mut accounts,
            privileges,
            vec![],
            program_id,
            &cache,
            0,
            100_000,
        );

        let result =
            WasmExecutor::execute(b"invalid wasm", &program_id, &[], &mut host_state, &cache);
        assert!(result.is_err());
    }

    #[test]
    fn empty_bytecode_fails() {
        let cache = ProgramCache::new(10);
        let program_id = hash(b"empty");
        let mut accounts = vec![];
        let privileges: &[(bool, bool)] = &[];
        let mut host_state = VmHostState::new(
            &mut accounts,
            privileges,
            vec![],
            program_id,
            &cache,
            0,
            100_000,
        );

        let result = WasmExecutor::execute(&[], &program_id, &[], &mut host_state, &cache);
        assert!(matches!(result.unwrap_err(), VmError::Compilation(_)));
    }

    #[test]
    fn insufficient_compute_for_instantiation() {
        let cache = ProgramCache::new(10);
        let program_id = hash(b"prog");
        let mut accounts = vec![];
        let privileges: &[(bool, bool)] = &[];
        // Give fewer compute units than COST_INSTANTIATION
        let mut host_state = VmHostState::new(
            &mut accounts,
            privileges,
            vec![],
            program_id,
            &cache,
            0,
            COST_INSTANTIATION - 1,
        );

        // Use a tiny valid wasm module header. The compute check now happens
        // BEFORE compilation (E6), so this should fail with ComputeExceeded.
        let wasm_header = [0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00];

        let result = WasmExecutor::execute(&wasm_header, &program_id, &[], &mut host_state, &cache);
        // Now fails with ComputeExceeded (charge is before compile).
        assert!(matches!(result.unwrap_err(), VmError::ComputeExceeded));
    }

    #[test]
    fn execute_valid_wasm_succeeds() {
        let wasm = wat_to_wasm(MINIMAL_WAT);
        let cache = ProgramCache::new(10);
        let program_id = hash(b"valid_program");
        let mut accounts = vec![];
        let privileges: &[(bool, bool)] = &[];
        let mut host_state = VmHostState::new(
            &mut accounts,
            privileges,
            vec![],
            program_id,
            &cache,
            0,
            1_000_000,
        );

        let result = WasmExecutor::execute(&wasm, &program_id, &[], &mut host_state, &cache);
        assert!(result.is_ok());
    }

    #[test]
    fn cache_hit_on_second_execution_of_same_bytecode() {
        let wasm = wat_to_wasm(MINIMAL_WAT);
        let cache = ProgramCache::new(10);
        let program_id = hash(b"cached_program");

        // First execution: cache miss, module gets compiled and inserted.
        {
            let mut accounts = vec![];
            let privileges: &[(bool, bool)] = &[];
            let mut host_state = VmHostState::new(
                &mut accounts,
                privileges,
                vec![],
                program_id,
                &cache,
                0,
                1_000_000,
            );

            let result = WasmExecutor::execute(&wasm, &program_id, &[], &mut host_state, &cache);
            assert!(result.is_ok());
        }

        // After first execution, cache should contain exactly one entry.
        assert_eq!(
            cache.len(),
            1,
            "module should be cached after first execution"
        );

        // The bytecode hash should be retrievable from the cache.
        let bytecode_hash = crypto_hash(&wasm);
        assert!(
            cache.get(&bytecode_hash).is_some(),
            "cache should be keyed by bytecode hash"
        );

        // Second execution with same bytecode: should hit the cache.
        {
            let mut accounts = vec![];
            let privileges: &[(bool, bool)] = &[];
            let mut host_state = VmHostState::new(
                &mut accounts,
                privileges,
                vec![],
                program_id,
                &cache,
                0,
                1_000_000,
            );

            let result = WasmExecutor::execute(&wasm, &program_id, &[], &mut host_state, &cache);
            assert!(result.is_ok());
        }

        // Cache should still contain exactly one entry (no duplicate).
        assert_eq!(
            cache.len(),
            1,
            "same bytecode should reuse cached module, not insert a duplicate"
        );
    }

    #[test]
    fn cache_invalidation_on_bytecode_change() {
        let wasm_v1 = wat_to_wasm(MINIMAL_WAT);
        let wasm_v2 = wat_to_wasm(DIFFERENT_WAT);
        let cache = ProgramCache::new(10);

        // Both versions are deployed to the same program address.
        let program_id = hash(b"upgradeable_program");

        // Execute with v1 bytecode.
        {
            let mut accounts = vec![];
            let privileges: &[(bool, bool)] = &[];
            let mut host_state = VmHostState::new(
                &mut accounts,
                privileges,
                vec![],
                program_id,
                &cache,
                0,
                1_000_000,
            );
            assert!(WasmExecutor::execute(&wasm_v1, &program_id, &[], &mut host_state, &cache).is_ok());
        }

        assert_eq!(cache.len(), 1);

        // Execute with v2 bytecode: different hash -> cache miss -> new entry.
        {
            let mut accounts = vec![];
            let privileges: &[(bool, bool)] = &[];
            let mut host_state = VmHostState::new(
                &mut accounts,
                privileges,
                vec![],
                program_id,
                &cache,
                0,
                1_000_000,
            );
            assert!(WasmExecutor::execute(&wasm_v2, &program_id, &[], &mut host_state, &cache).is_ok());
        }

        assert_eq!(cache.len(), 2, "different bytecodes should be separate cache entries");

        // Both hashes should be independently retrievable.
        let hash_v1 = crypto_hash(&wasm_v1);
        let hash_v2 = crypto_hash(&wasm_v2);
        assert!(cache.get(&hash_v1).is_some());
        assert!(cache.get(&hash_v2).is_some());
    }

    #[test]
    fn heap_initialized_to_heap_start() {
        // Verify E4 fix: heap_offset starts at 0 in VmHostState but the
        // executor must initialize it to HEAP_START before writing.
        let wasm = wat_to_wasm(MINIMAL_WAT);
        let cache = ProgramCache::new(10);
        let program_id = hash(b"heap_test");
        let mut accounts = vec![];
        let privileges: &[(bool, bool)] = &[];
        let mut host_state = VmHostState::new(
            &mut accounts,
            privileges,
            vec![],
            program_id,
            &cache,
            0,
            1_000_000,
        );

        // Before execution heap_offset is 0.
        assert_eq!(host_state.heap_offset, 0);

        let result = WasmExecutor::execute(&wasm, &program_id, &[], &mut host_state, &cache);
        assert!(result.is_ok());

        // After execution heap_offset should be at or above HEAP_START.
        assert!(
            host_state.heap_offset >= HEAP_START,
            "heap_offset ({}) must be >= HEAP_START ({})",
            host_state.heap_offset,
            HEAP_START
        );
    }
}
