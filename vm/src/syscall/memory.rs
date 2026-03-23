//! Heap memory syscall: `nusa_alloc` (bump allocator).
//!
//! WASM programs use a simple bump allocator for dynamic memory. The heap
//! occupies a fixed region of the linear memory starting at [`HEAP_START`]
//! with a size of [`HEAP_SIZE`] bytes. Allocations are never freed during a
//! single program invocation -- the entire heap is reclaimed when the
//! invocation completes.

use wasmi::Linker;

use crate::error::VmError;

/// Start of the bump-allocator heap in WASM linear memory (3 MiB offset,
/// after the stack region).
const HEAP_START: u32 = 0x0030_0000;

/// Total heap size available for bump allocation (1 MiB).
const HEAP_SIZE: u32 = 0x0010_0000;

/// Allocate `size` bytes from the WASM heap.
///
/// Returns the start offset of the allocated region. The `heap_offset`
/// is advanced past the allocation. Returns [`VmError::HeapExhausted`]
/// if the allocation would exceed the heap.
pub fn heap_alloc(size: u32, heap_offset: &mut u32) -> Result<u32, VmError> {
    let start = if *heap_offset == 0 {
        HEAP_START
    } else {
        *heap_offset
    };

    let end = start.checked_add(size).ok_or(VmError::HeapExhausted {
        need: size,
        available: 0,
    })?;

    if end > HEAP_START + HEAP_SIZE {
        return Err(VmError::HeapExhausted {
            need: size,
            available: (HEAP_START + HEAP_SIZE).saturating_sub(start),
        });
    }

    *heap_offset = end;
    Ok(start)
}

/// Register the `nusa_alloc` syscall in the linker.
///
/// Currently a stub that always returns 0 (allocation failure) because the
/// `Store<()>` type does not carry `VmHostState`. Once the store is upgraded
/// this will call [`heap_alloc`] with the real heap offset.
pub fn register(linker: &mut Linker<()>) -> Result<(), VmError> {
    linker
        .func_wrap("env", "nusa_alloc", |_size: i32| -> i32 {
            0 // Stub: full implementation delegates to heap_alloc
        })
        .map_err(|e| VmError::Syscall(e.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_alloc_starts_at_heap_start() {
        let mut offset = 0u32;
        let ptr = heap_alloc(100, &mut offset).unwrap();
        assert_eq!(ptr, HEAP_START);
        assert_eq!(offset, HEAP_START + 100);
    }

    #[test]
    fn sequential_allocs_are_contiguous() {
        let mut offset = 0u32;
        let ptr1 = heap_alloc(100, &mut offset).unwrap();
        let ptr2 = heap_alloc(200, &mut offset).unwrap();
        assert_eq!(ptr1, HEAP_START);
        assert_eq!(ptr2, HEAP_START + 100);
        assert_eq!(offset, HEAP_START + 300);
    }

    #[test]
    fn zero_size_alloc() {
        let mut offset = 0u32;
        let ptr = heap_alloc(0, &mut offset).unwrap();
        assert_eq!(ptr, HEAP_START);
        assert_eq!(offset, HEAP_START);
    }

    #[test]
    fn exact_fit() {
        let mut offset = 0u32;
        let ptr = heap_alloc(HEAP_SIZE, &mut offset).unwrap();
        assert_eq!(ptr, HEAP_START);
        assert_eq!(offset, HEAP_START + HEAP_SIZE);
    }

    #[test]
    fn heap_exhausted_single_large() {
        let mut offset = 0u32;
        let result = heap_alloc(HEAP_SIZE + 1, &mut offset);
        assert!(matches!(result.unwrap_err(), VmError::HeapExhausted { .. }));
    }

    #[test]
    fn heap_exhausted_cumulative() {
        let mut offset = 0u32;
        heap_alloc(HEAP_SIZE - 10, &mut offset).unwrap();
        let result = heap_alloc(11, &mut offset);
        assert!(matches!(result.unwrap_err(), VmError::HeapExhausted { .. }));
    }

    #[test]
    fn heap_exhausted_reports_available() {
        let mut offset = 0u32;
        heap_alloc(HEAP_SIZE - 100, &mut offset).unwrap();
        match heap_alloc(200, &mut offset) {
            Err(VmError::HeapExhausted { need, available }) => {
                assert_eq!(need, 200);
                assert_eq!(available, 100);
            }
            other => panic!("expected HeapExhausted, got: {other:?}"),
        }
    }
}
