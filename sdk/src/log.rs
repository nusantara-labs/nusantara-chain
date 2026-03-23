//! Logging utilities for Nusantara programs.
//!
//! Programs emit log messages via the `nusa_log` syscall. These messages appear
//! in transaction logs and are visible through the RPC API. The `msg!` macro is
//! the recommended interface; `log_message` and `log_compute_units` are the
//! lower-level building blocks.

/// Log a formatted message from a program.
///
/// Under WASM this calls the `nusa_log` syscall. Outside WASM (for testing) it
/// prints to stdout.
///
/// # Usage
///
/// ```
/// # use nusantara_sdk::msg;
/// msg!("Hello, world!");
/// msg!("Counter value: {}", 42);
/// ```
#[macro_export]
macro_rules! msg {
    ($($arg:tt)*) => {
        {
            let message = format!($($arg)*);
            $crate::log::log_message(&message);
        }
    };
}

/// Send a log message to the VM.
///
/// Under `wasm32` this calls the `nusa_log` host function. Outside WASM it
/// prints to stdout, prefixed with `"Program log: "`.
pub fn log_message(message: &str) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        crate::syscall::nusa_log(message.as_ptr(), message.len() as i32);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        println!("Program log: {message}");
    }
}

/// Log the number of compute units remaining.
///
/// Useful for profiling gas consumption within a program. Under WASM this
/// calls the `nusa_log_compute_units` host function.
pub fn log_compute_units() {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        crate::syscall::nusa_log_compute_units();
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        println!("Program log: compute units remaining");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_message_does_not_panic() {
        log_message("test message");
    }

    #[test]
    fn log_compute_units_does_not_panic() {
        log_compute_units();
    }

    #[test]
    fn msg_macro_does_not_panic() {
        msg!("formatted: {}", 123);
    }
}
