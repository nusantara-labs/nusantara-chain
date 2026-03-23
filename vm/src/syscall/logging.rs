//! Logging syscalls: `nusa_log` and `nusa_log_compute_units`.
//!
//! WASM programs emit log messages through these syscalls. Messages are
//! forwarded to the tracing infrastructure and collected in the host state
//! for inclusion in transaction metadata.

use tracing::info;
use wasmi::Linker;

use crate::config::{COST_LOG_BASE, MAX_LOG_MESSAGE_SIZE};
use crate::error::VmError;

/// Log a message from a WASM program.
///
/// The message is emitted via [`tracing::info!`] with the program identity
/// as a span field, and the `nusantara_vm_log_messages` counter is incremented.
///
/// Returns [`VmError::LogMessageTooLarge`] if the message exceeds the limit.
pub fn log_message(message: &str, program_id: &str) -> Result<(), VmError> {
    if message.len() > MAX_LOG_MESSAGE_SIZE {
        return Err(VmError::LogMessageTooLarge {
            size: message.len(),
            max: MAX_LOG_MESSAGE_SIZE,
        });
    }
    info!(program = program_id, "Program log: {}", message);
    metrics::counter!("nusantara_vm_log_messages").increment(1);
    Ok(())
}

/// Calculate the compute-unit cost for logging a message.
///
/// Cost scales linearly with message length.
pub fn log_cost(message_len: usize) -> u64 {
    COST_LOG_BASE + message_len as u64
}

/// Register logging syscalls in the linker.
///
/// Currently registers simplified stubs because the `Store<()>` type does not
/// carry `VmHostState`. When the store is upgraded to `Store<VmHostState>`,
/// these will read from WASM linear memory and forward to [`log_message`].
pub fn register(linker: &mut Linker<()>) -> Result<(), VmError> {
    // nusa_log(ptr: i32, len: i32) -> ()
    linker
        .func_wrap("env", "nusa_log", |_ptr: i32, _len: i32| {
            // Stub: full implementation reads UTF-8 from WASM memory at [ptr..ptr+len]
        })
        .map_err(|e| VmError::Syscall(e.to_string()))?;

    // nusa_log_compute_units() -> ()
    linker
        .func_wrap("env", "nusa_log_compute_units", || {
            // Stub: full implementation logs remaining compute units
        })
        .map_err(|e| VmError::Syscall(e.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_normal_message() {
        assert!(log_message("hello world", "test_program").is_ok());
    }

    #[test]
    fn log_empty_message() {
        assert!(log_message("", "test_program").is_ok());
    }

    #[test]
    fn log_at_limit() {
        let msg = "x".repeat(MAX_LOG_MESSAGE_SIZE);
        assert!(log_message(&msg, "test_program").is_ok());
    }

    #[test]
    fn log_too_large() {
        let msg = "x".repeat(MAX_LOG_MESSAGE_SIZE + 1);
        let err = log_message(&msg, "test_program").unwrap_err();
        assert!(matches!(err, VmError::LogMessageTooLarge { .. }));
    }

    #[test]
    fn cost_scales_with_length() {
        assert_eq!(log_cost(0), COST_LOG_BASE);
        assert_eq!(log_cost(100), COST_LOG_BASE + 100);
        assert_eq!(log_cost(1), COST_LOG_BASE + 1);
    }
}
