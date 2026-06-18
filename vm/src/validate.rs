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

use wasmi::{Engine, ExternType, FuncType, Module, ValType};

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

    // 6. Start-function detection: wasmi 1.x removed the `InstancePre`
    //    pre-instantiation API and exposes no public accessor for a module's
    //    start section, so we scan the raw WASM section table ourselves and
    //    reject any module that declares a start function.
    if has_start_section(bytecode) {
        return Err(VmError::HasStartFunction);
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
}
