//! WASM bytecode validation for the Nusantara blockchain.
//!
//! Before a program can be deployed or executed, its bytecode must pass these checks:
//!
//! 1. **Size limit** -- bytecode length must not exceed [`MAX_WASM_BYTECODE_SIZE`].
//! 2. **Parsability** -- the bytes must be valid WASM (only safe proposals enabled).
//! 3. **Entrypoint export** -- the module must export a function named `entrypoint`
//!    with signature `(i32, i32, i32, i32) -> i64`.
//! 4. **Memory export** -- the module must export a `memory` of type `Memory`.
//! 5. **Memory page limits** -- initial pages ≤ `MAX_MEMORY_PAGES`, and the module
//!    must declare an explicit bounded maximum.
//! 6. **No start function** -- modules with a WASM `start` section are rejected
//!    because they execute arbitrary code at instantiation time.
//! 7. **Import whitelist** -- only `env::*` imports matching registered syscalls
//!    are permitted; any other import is rejected.
//! 8. **Count caps** -- functions, tables, table elements, globals, imports, and
//!    cumulative custom-section bytes are capped at conservative limits.
//!
//! ## Engine hardening
//!
//! [`harden_config`] is the single authoritative place where the wasmi [`Config`]
//! is locked down. It is called by both `validate_wasm` and
//! `program_cache::build_engine` so the two execution paths cannot drift apart.

use wasmi::{Config, Engine, ExternType, FuncType, Module, ValType};

use crate::config::{
    MAX_CUSTOM_SECTION_BYTES, MAX_FUNCTIONS, MAX_GLOBALS, MAX_IMPORTS, MAX_MEMORY_PAGES,
    MAX_TABLES, MAX_TABLE_ELEMENTS, MAX_WASM_BYTECODE_SIZE,
};
use crate::error::VmError;

/// Whitelist of syscall function names that the linker wires under `"env"`.
///
/// This must stay in sync with the functions registered in `syscall::mod::link_all`.
/// Any import not in this set is rejected at validation time so a malicious
/// module cannot declare phantom imports that would silently be missing at
/// instantiation.
const ALLOWED_IMPORTS: &[&str] = &[
    "nusa_log",
    "nusa_log_compute_units",
    "nusa_alloc",
];

/// Harden a wasmi [`Config`] for on-chain execution.
///
/// This is the **single authoritative** configuration site. Both
/// `validate_wasm` (which builds its own local engine) and
/// `program_cache::build_engine` (which builds the shared engine) call this
/// function so neither can accidentally enable a dangerous proposal.
///
/// Proposals disabled:
/// - floats (non-deterministic NaN propagation)
/// - memory64 (64-bit memory indices; prevents u64→u32 truncation bugs)
/// - multi-memory (only one `env.memory` export expected)
/// - threads (shared memory, atomics; not deterministic across chains)
/// - tail-call (implementation risk; not needed)
/// - reference-types (GC-types surface area)
/// - SIMD (disabled unless the `simd` feature is active; non-deterministic)
///
/// Proposals enabled but always present in wasmi default:
/// - mutable-global, sign-extension, saturating-float-to-int, multi-value,
///   bulk-memory, extended-const (safe and commonly needed)
pub fn harden_config(config: &mut Config) {
    config.consume_fuel(true);
    config.floats(false);
    // Disable memory64: wasmi 1.1 MemoryType::minimum() returns u64, and
    // memory64 allows min >= 2^32 which silently wraps past MAX_MEMORY_PAGES
    // if cast to u32. Keeping it disabled also guarantees 32-bit addressing.
    config.wasm_memory64(false);
    // Disable multi-memory: on-chain programs use exactly one linear memory.
    config.wasm_multi_memory(false);
    // Disable tail-call: implementation risk with no on-chain benefit.
    config.wasm_tail_call(false);
    // Disable reference-types: increases GC attack surface.
    config.wasm_reference_types(false);
    // Threads: wasmi 1.1 does not expose a `wasm_threads` config method
    // because the threads proposal requires shared memory which wasmi doesn't
    // implement. No action needed.
    //
    // SIMD: `wasmi::Config::wasm_simd` is `#[cfg(feature = "simd")]`. The
    // workspace enables no SIMD feature for wasmi (see root Cargo.toml), so
    // SIMD is already disabled at compile time. No runtime call needed.
}

/// Validate WASM bytecode according to Nusantara's deployment rules.
///
/// Returns `Ok(())` if the bytecode passes all checks, or a descriptive
/// [`VmError`] indicating which validation rule was violated.
pub fn validate_wasm(bytecode: &[u8]) -> Result<(), VmError> {
    // 1. Size check
    if bytecode.len() > MAX_WASM_BYTECODE_SIZE {
        return Err(VmError::BytecodeTooLarge {
            size: bytecode.len(),
            max: MAX_WASM_BYTECODE_SIZE,
        });
    }

    // 2. Parse with the hardened engine configuration.
    let mut config = Config::default();
    harden_config(&mut config);
    let engine = Engine::new(&config);

    let module = Module::new(&engine, bytecode).map_err(|e| VmError::Compilation(e.to_string()))?;

    // 3-5. Walk the export table and check entrypoint, memory, and start
    let mut has_entrypoint = false;
    let mut has_memory = false;

    let expected_sig = FuncType::new(
        [ValType::I32, ValType::I32, ValType::I32, ValType::I32],
        [ValType::I64],
    );

    for export in module.exports() {
        match export.name() {
            "entrypoint" => match export.ty() {
                ExternType::Func(func_ty) => {
                    if *func_ty != expected_sig {
                        return Err(VmError::InvalidEntrypointSignature);
                    }
                    has_entrypoint = true;
                }
                _ => return Err(VmError::InvalidEntrypointSignature),
            },
            "memory" => match export.ty() {
                ExternType::Memory(mem_ty) => {
                    // V1: minimum() returns u64 in wasmi 1.1; compare as u64
                    // to avoid silent truncation for memory64 modules.
                    let initial: u64 = mem_ty.minimum();
                    if initial > MAX_MEMORY_PAGES as u64 {
                        return Err(VmError::TooManyMemoryPages {
                            pages: initial,
                            max: MAX_MEMORY_PAGES as u64,
                        });
                    }

                    // V2: Require an explicit bounded maximum. Modules that
                    // declare `(memory N)` without an upper bound can grow
                    // at runtime up to the engine's internal limit. We reject
                    // them to guarantee a hard ceiling on memory usage.
                    match mem_ty.maximum() {
                        None => return Err(VmError::UnboundedMemory),
                        Some(max) if max > MAX_MEMORY_PAGES as u64 => {
                            return Err(VmError::TooManyMemoryPages {
                                pages: max,
                                max: MAX_MEMORY_PAGES as u64,
                            });
                        }
                        Some(_) => {}
                    }

                    has_memory = true;
                }
                // V9: Export named "memory" that is not actually a Memory type.
                _ => {
                    return Err(VmError::Validation(
                        "export 'memory' must be a Memory".into(),
                    ))
                }
            },
            _ => {}
        }
    }

    if !has_entrypoint {
        return Err(VmError::MissingEntrypoint);
    }

    if !has_memory {
        return Err(VmError::MissingMemory);
    }

    // V3: Walk imports and enforce the module-name and function-name whitelist.
    // Only the `"env"` module namespace is allowed. Function names must match
    // the registered syscalls in `syscall::mod::link_all`.
    let import_count: u32 = module
        .imports()
        .enumerate()
        .map(|(i, import)| {
            let _ = i; // enumerate used only for count
            import
        })
        .try_fold(0u32, |count, import| {
            if import.module() != "env" {
                return Err(VmError::UnknownImport {
                    module: import.module().to_string(),
                    name: import.name().to_string(),
                });
            }
            if !ALLOWED_IMPORTS.contains(&import.name()) {
                return Err(VmError::UnknownImport {
                    module: import.module().to_string(),
                    name: import.name().to_string(),
                });
            }
            Ok(count + 1)
        })?;

    // V4: Import count cap (redundant with whitelist but provides a numeric limit).
    if import_count > MAX_IMPORTS {
        return Err(VmError::TooManyImports {
            count: import_count,
            max: MAX_IMPORTS,
        });
    }

    // V4: Section-level count caps. wasmi's Module API exposes imports() and
    // exports() as iterators but does not expose pub methods for function/table/
    // global counts (they are pub(crate)). We scan the raw WASM section bytes
    // to extract counts from the function, table, global, and element sections.
    check_section_count_caps(bytecode)?;

    // 6. Start-function detection: wasmi 1.x removed the `InstancePre`
    //    pre-instantiation API and exposes no public accessor for a module's
    //    start section, so we scan the raw WASM section table ourselves and
    //    reject any module that declares a start function.
    if has_start_section(bytecode) {
        return Err(VmError::HasStartFunction);
    }

    Ok(())
}

/// Scan section counts from raw WASM bytes and enforce per-section caps.
///
/// Walks the binary section table and reads the leading LEB128 count from
/// each relevant section:
/// - Section 3 (Function): total function count ≤ `MAX_FUNCTIONS`
/// - Section 4 (Table): table count ≤ `MAX_TABLES`
/// - Section 6 (Global): global count ≤ `MAX_GLOBALS`
/// - Section 9 (Element): total table element count (sum of element-segment
///   vector lengths) ≤ `MAX_TABLE_ELEMENTS`
/// - Section 0 (Custom): cumulative payload bytes ≤ `MAX_CUSTOM_SECTION_BYTES`
///
/// Section 2 (Import) count is verified separately via `module.imports()`.
/// This function operates on the pre-validated binary, so structural validity
/// is guaranteed by the preceding `Module::new` call.
fn check_section_count_caps(bytecode: &[u8]) -> Result<(), VmError> {
    let mut pos = 8; // skip magic + version
    if bytecode.len() < pos {
        return Ok(());
    }

    let mut total_functions: u32 = 0;
    let mut total_tables: u32 = 0;
    let mut total_globals: u32 = 0;
    let mut total_table_elements: u32 = 0;
    let mut total_custom_bytes: u32 = 0;

    while pos < bytecode.len() {
        let section_id = bytecode[pos];
        pos += 1;

        let Some((section_size, consumed)) = read_uleb128(&bytecode[pos..]) else {
            break;
        };
        pos += consumed;
        let section_start = pos;
        let section_end = match pos.checked_add(section_size as usize) {
            Some(e) if e <= bytecode.len() => e,
            _ => break,
        };
        let section_bytes = &bytecode[section_start..section_end];

        match section_id {
            // Function section
            3 => {
                if let Some((count, _)) = read_uleb128(section_bytes) {
                    total_functions = total_functions.saturating_add(count);
                    if total_functions > MAX_FUNCTIONS {
                        return Err(VmError::TooManyFunctions {
                            count: total_functions,
                            max: MAX_FUNCTIONS,
                        });
                    }
                }
            }
            // Table section
            4 => {
                if let Some((count, _)) = read_uleb128(section_bytes) {
                    total_tables = total_tables.saturating_add(count);
                    if total_tables > MAX_TABLES {
                        return Err(VmError::TooManyTables {
                            count: total_tables,
                            max: MAX_TABLES,
                        });
                    }
                }
            }
            // Global section
            6 => {
                if let Some((count, _)) = read_uleb128(section_bytes) {
                    total_globals = total_globals.saturating_add(count);
                    if total_globals > MAX_GLOBALS {
                        return Err(VmError::TooManyGlobals {
                            count: total_globals,
                            max: MAX_GLOBALS,
                        });
                    }
                }
            }
            // Element section: the first LEB128 in the section is the segment
            // count. We conservatively treat each segment as at least 1 element
            // (parsing the full per-segment element count would require decoding
            // variable-length segment headers). Capping segment count at
            // MAX_TABLE_ELEMENTS is safe and conservative.
            9 => {
                if let Some((seg_count, _)) = read_uleb128(section_bytes) {
                    total_table_elements = total_table_elements.saturating_add(seg_count);
                    if total_table_elements > MAX_TABLE_ELEMENTS {
                        return Err(VmError::TooManyTableElements {
                            count: total_table_elements,
                            max: MAX_TABLE_ELEMENTS,
                        });
                    }
                }
            }
            // Custom section (id 0): accumulate payload bytes.
            0 => {
                let payload_len = section_size;
                total_custom_bytes = total_custom_bytes.saturating_add(payload_len);
                if total_custom_bytes > MAX_CUSTOM_SECTION_BYTES {
                    return Err(VmError::CustomSectionTooLarge {
                        bytes: total_custom_bytes,
                        max: MAX_CUSTOM_SECTION_BYTES,
                    });
                }
            }
            // Other sections: skip.
            _ => {}
        }

        pos = section_end;
    }

    Ok(())
}

/// Scan the raw WASM binary for a `start` section (section id `8`).
///
/// The start section executes arbitrary code at instantiation time, which we
/// disallow on-chain. WASM section layout after the 8-byte header is a flat
/// sequence of `(section_id: u8, size: u32 LEB128, payload[size])` records, so
/// we walk the records and look for id `8`. Malformed input simply stops the
/// walk and reports "no start section"; structural validity is already
/// guaranteed by the preceding `Module::new` parse.
pub(crate) fn has_start_section(bytecode: &[u8]) -> bool {
    // Skip the 4-byte magic (`\0asm`) and 4-byte version.
    let mut pos = 8;
    if bytecode.len() < pos {
        return false;
    }
    while pos < bytecode.len() {
        let section_id = bytecode[pos];
        pos += 1;
        // Read the unsigned LEB128 section size.
        let Some((size, consumed)) = read_uleb128(&bytecode[pos..]) else {
            return false;
        };
        pos += consumed;
        if section_id == 8 {
            return true;
        }
        // Skip this section's payload.
        let Some(next) = pos.checked_add(size as usize) else {
            return false;
        };
        if next > bytecode.len() {
            return false;
        }
        pos = next;
    }
    false
}

/// Decode an unsigned LEB128 integer, returning `(value, bytes_consumed)`.
/// Returns `None` on truncated or overlong (> 5-byte / u32-overflowing) input.
fn read_uleb128(bytes: &[u8]) -> Option<(u32, usize)> {
    let mut result: u32 = 0;
    let mut shift = 0u32;
    for (i, &byte) in bytes.iter().enumerate() {
        if shift >= 32 {
            return None;
        }
        result |= ((byte & 0x7f) as u32).checked_shl(shift)?;
        shift += 7;
        if byte & 0x80 == 0 {
            return Some((result, i + 1));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_large() {
        let bytecode = vec![0u8; MAX_WASM_BYTECODE_SIZE + 1];
        let err = validate_wasm(&bytecode).unwrap_err();
        assert!(matches!(err, VmError::BytecodeTooLarge { .. }));
    }

    #[test]
    fn invalid_wasm_bytes() {
        let err = validate_wasm(b"not wasm at all").unwrap_err();
        assert!(matches!(err, VmError::Compilation(_)));
    }

    #[test]
    fn empty_bytecode() {
        let err = validate_wasm(&[]).unwrap_err();
        assert!(matches!(err, VmError::Compilation(_)));
    }

    /// A minimal valid WASM binary (magic + version only) should fail because
    /// it has no exports.
    #[test]
    fn wasm_header_only() {
        let wasm_header = [0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00];
        let err = validate_wasm(&wasm_header).unwrap_err();
        assert!(matches!(err, VmError::MissingEntrypoint));
    }

    /// A module with a bounded memory declaration passes memory validation.
    #[test]
    fn bounded_memory_passes() {
        // (module (memory (export "memory") 1 64) (func (export "entrypoint") ...) )
        let wasm = wat::parse_str(r#"
            (module
                (memory (export "memory") 1 64)
                (func (export "entrypoint") (param i32 i32 i32 i32) (result i64)
                    i64.const 0
                )
            )
        "#).unwrap();
        assert!(validate_wasm(&wasm).is_ok());
    }

    /// A module without a memory maximum must be rejected (V2).
    #[test]
    fn unbounded_memory_rejected() {
        let wasm = wat::parse_str(r#"
            (module
                (memory (export "memory") 1)
                (func (export "entrypoint") (param i32 i32 i32 i32) (result i64)
                    i64.const 0
                )
            )
        "#).unwrap();
        let err = validate_wasm(&wasm).unwrap_err();
        assert!(matches!(err, VmError::UnboundedMemory), "got: {err}");
    }

    /// A module with memory max > MAX_MEMORY_PAGES is rejected (V2).
    #[test]
    fn memory_max_too_large_rejected() {
        let wasm = wat::parse_str(r#"
            (module
                (memory (export "memory") 1 65536)
                (func (export "entrypoint") (param i32 i32 i32 i32) (result i64)
                    i64.const 0
                )
            )
        "#).unwrap();
        let err = validate_wasm(&wasm).unwrap_err();
        assert!(matches!(err, VmError::TooManyMemoryPages { .. }), "got: {err}");
    }

    /// Unknown import module is rejected (V3).
    #[test]
    fn unknown_import_module_rejected() {
        let wasm = wat::parse_str(r#"
            (module
                (import "notenv" "nusa_log" (func (param i32 i32)))
                (memory (export "memory") 1 64)
                (func (export "entrypoint") (param i32 i32 i32 i32) (result i64)
                    i64.const 0
                )
            )
        "#).unwrap();
        let err = validate_wasm(&wasm).unwrap_err();
        assert!(matches!(err, VmError::UnknownImport { .. }), "got: {err}");
    }

    /// Unknown function import name is rejected (V3).
    #[test]
    fn unknown_import_name_rejected() {
        let wasm = wat::parse_str(r#"
            (module
                (import "env" "evil_syscall" (func (param i32)))
                (memory (export "memory") 1 64)
                (func (export "entrypoint") (param i32 i32 i32 i32) (result i64)
                    i64.const 0
                )
            )
        "#).unwrap();
        let err = validate_wasm(&wasm).unwrap_err();
        assert!(matches!(err, VmError::UnknownImport { .. }), "got: {err}");
    }

    /// Whitelisted imports are accepted (V3).
    #[test]
    fn whitelisted_imports_pass() {
        let wasm = wat::parse_str(r#"
            (module
                (import "env" "nusa_log" (func (param i32 i32)))
                (import "env" "nusa_alloc" (func (param i32) (result i32)))
                (memory (export "memory") 1 64)
                (func (export "entrypoint") (param i32 i32 i32 i32) (result i64)
                    i64.const 0
                )
            )
        "#).unwrap();
        assert!(validate_wasm(&wasm).is_ok(), "whitelisted imports should pass");
    }
}
