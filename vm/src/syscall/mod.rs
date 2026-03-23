//! Host-function (syscall) registration for the Nusantara WASM VM.
//!
//! Each sub-module defines a family of syscalls that WASM programs can call
//! via the `env` import namespace. Syscalls are the only way for a WASM
//! program to interact with blockchain state (accounts, sysvars, logging,
//! crypto operations, and cross-program invocations).
//!
//! ## Architecture note
//!
//! In the current implementation the wasmi [`Store`] is typed as `Store<()>`
//! because the full [`VmHostState`] lives outside the store and is accessed
//! through the executor's call frame. Syscalls that require host-state access
//! (account reads, CPI, etc.) are implemented as free functions that the
//! executor invokes when translating WASM memory buffers. The linker-registered
//! functions are minimal stubs that will be connected to the real host state
//! as the architecture evolves.

pub mod account;
pub mod auth;
pub mod cpi;
pub mod crypto;
pub mod logging;
pub mod memory;
pub mod return_data;
pub mod sysvar;

use wasmi::{Engine, Linker};

use crate::error::VmError;

/// Register all host functions (syscalls) in the linker.
///
/// This wires up the `env.*` imports that WASM modules expect to find at
/// instantiation time. Currently registers logging and memory allocator
/// stubs. Account, auth, crypto, sysvar, CPI, and return-data syscalls
/// are implemented as free functions and will be wired into the linker
/// once the store type carries `VmHostState`.
pub fn link_all(linker: &mut Linker<()>, _engine: &Engine) -> Result<(), VmError> {
    logging::register(linker)?;
    memory::register(linker)?;
    Ok(())
}
