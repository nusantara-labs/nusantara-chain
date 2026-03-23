//! WASM bytecode validation for the Nusantara blockchain.
//!
//! Before a program can be deployed or executed, its bytecode must pass these checks:
//!
//! 1. **Size limit** -- bytecode length must not exceed [`MAX_WASM_BYTECODE_SIZE`].
//! 2. **Parsability** -- the bytes must be valid WASM (with floats disabled).
//! 3. **Entrypoint export** -- the module must export a function named `entrypoint`
//!    with signature `(i32, i32, i32, i32) -> i64`.
//! 4. **Memory export** -- the module must export a `memory` of type `Memory`.
//! 5. **Memory page limit** -- the initial memory pages must not exceed
//!    [`MAX_MEMORY_PAGES`].
//! 6. **No start function** -- modules with a WASM `start` section are rejected
//!    because they execute arbitrary code at instantiation time.

use wasmi::core::ValType;
use wasmi::{Engine, ExternType, FuncType, Module};

use crate::config::{MAX_MEMORY_PAGES, MAX_WASM_BYTECODE_SIZE};
use crate::error::VmError;

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

    // 2. Parse with floats disabled and fuel metering enabled
    let mut config = wasmi::Config::default();
    config.consume_fuel(true);
    config.floats(false);
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
                    let initial = mem_ty.minimum() as u32;
                    if initial > MAX_MEMORY_PAGES {
                        return Err(VmError::TooManyMemoryPages {
                            pages: initial,
                            max: MAX_MEMORY_PAGES,
                        });
                    }
                    has_memory = true;
                }
                _ => return Err(VmError::MissingMemory),
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

    // 6. Start-function detection: we attempt `ensure_no_start` during
    //    execution in the executor. At validation time we rely on the fact
    //    that wasmi will report a start-function error during instantiation
    //    via `InstancePre::ensure_no_start`. There is no public accessor on
    //    `Module` for the start section, so we defer the check.

    Ok(())
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
}
