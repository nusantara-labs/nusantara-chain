pub mod account;
pub mod accounts_by;
pub mod block;
pub mod epoch;
pub mod faucet;
pub mod health;
pub mod jsonrpc_dispatch;
pub mod leader;
pub mod program;
pub mod proof;
pub mod signatures;
pub mod slot;
pub mod snapshot;
pub mod snapshot_download;
pub mod stake;
pub mod transaction;
pub mod validator;
pub mod vote;
pub mod ws;

/// Clamp an optional limit to a default and maximum value.
///
/// Used in handlers that accept a `?limit=N` query parameter to avoid
/// duplicating the `opt.unwrap_or(default).min(max)` pattern (F33).
#[inline]
pub(crate) fn clamp_limit(opt: Option<usize>, default: usize, max: usize) -> usize {
    opt.unwrap_or(default).min(max)
}
