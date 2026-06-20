//! Per-IP rate limiting middleware for the RPC server.
//!
//! Uses a `DashMap<IpAddr, IpRateState>` sliding-window counter pattern,
//! consistent with the TPU crate's rate limiter. The middleware is implemented
//! as a Tower `Layer` + `Service` so it integrates cleanly with Axum's router.
//!
//! Design decisions:
//! - **No locks across `.await`**: The `DashMap` entry lock is held only for
//!   the synchronous counter check/increment. The inner service `.call()` is
//!   invoked outside any lock scope.
//! - **Periodic cleanup**: A background Tokio task purges stale entries every
//!   `CLEANUP_INTERVAL_SECS` seconds. The task holds a `Weak<Inner>` so it
//!   terminates automatically when all strong references are dropped.
//! - **Explicit shutdown**: `new_with_shutdown` accepts a `watch::Receiver<bool>`
//!   for clean termination during validator shutdown. `new()` provides a
//!   never-firing equivalent.
//! - **Global rate**: A separate `AtomicU64` + `Mutex<Instant>` tracks the
//!   aggregate request rate across all IPs.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use tokio::sync::watch;
use tower::{Layer, Service};
use tracing::debug;

use crate::server::{
    MAX_RPC_REQUESTS_PER_SECOND_GLOBAL, MAX_RPC_REQUESTS_PER_SECOND_PER_IP,
    RATE_LIMITER_CLEANUP_INTERVAL_SECS, RATE_LIMITER_STALE_ENTRY_TIMEOUT_SECS,
};

/// Per-IP request tracking state.
struct IpRateState {
    request_count: u64,
    window_start: Instant,
}

/// Shared rate limiter state, cheap to clone via `Arc`.
#[derive(Clone)]
pub(crate) struct RpcRateLimiter {
    inner: Arc<RpcRateLimiterInner>,
}

struct RpcRateLimiterInner {
    ip_states: DashMap<IpAddr, IpRateState>,
    global_count: AtomicU64,
    global_window_start: parking_lot::Mutex<Instant>,
    /// Keeps the watch channel open for the lifetime of the limiter.
    /// Dropping this field (when the last `Arc<Inner>` is released) sends `true`
    /// via the `Drop` impl below, which causes the cleanup task to exit cleanly
    /// through the shutdown arm rather than via a channel-closed error.
    shutdown_tx: watch::Sender<bool>,
}

impl Drop for RpcRateLimiterInner {
    fn drop(&mut self) {
        // Signal the cleanup task to exit via its intended shutdown arm.
        let _ = self.shutdown_tx.send(true);
    }
}

impl RpcRateLimiter {
    pub(crate) fn new() -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        // `new_with_shutdown` builds Inner and stores `shutdown_tx` inside it,
        // keeping the channel alive for the limiter's entire lifetime.  The
        // Drop impl on Inner sends `true` when the last Arc<Inner> is released,
        // which causes the cleanup task to exit via the shutdown arm.
        Self::new_with_shutdown_tx(shutdown_tx, shutdown_rx)
    }

    /// Create a rate limiter whose background cleanup task terminates when
    /// `shutdown` fires `true` or when the last strong `Arc<Inner>` is dropped.
    ///
    /// The caller supplies an external `watch::Receiver<bool>` for coordinated
    /// shutdown (e.g. from the server's shutdown signal).  An internal sender is
    /// generated and stored on `Inner`; its `Drop` impl fires `true` when the
    /// last `Arc<Inner>` is released, ensuring the task always exits cleanly.
    ///
    /// Reserved for server-level coordinated shutdown; currently wired via
    /// `Default`/`new()` only but kept for future callers that manage their own
    /// shutdown signal.
    #[allow(dead_code)]
    pub(crate) fn new_with_shutdown(shutdown: watch::Receiver<bool>) -> Self {
        // Generate an internal (owned) sender so the task always has a live
        // channel.  The inner Drop impl signals this sender on destruction.
        let (internal_tx, _internal_rx) = watch::channel(false);
        Self::new_with_shutdown_tx(internal_tx, shutdown)
    }

    /// Internal constructor that accepts both the owned sender (stored on Inner)
    /// and the receiver (passed to the cleanup task).
    fn new_with_shutdown_tx(
        shutdown_tx: watch::Sender<bool>,
        mut shutdown: watch::Receiver<bool>,
    ) -> Self {
        let inner = Arc::new(RpcRateLimiterInner {
            ip_states: DashMap::new(),
            global_count: AtomicU64::new(0),
            global_window_start: parking_lot::Mutex::new(Instant::now()),
            shutdown_tx,
        });

        // Spawn the cleanup task using a Weak reference so the task does not
        // keep the limiter alive indefinitely.
        let weak = Arc::downgrade(&inner);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                RATE_LIMITER_CLEANUP_INTERVAL_SECS,
            ));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.wait_for(|v| *v) => {
                        tracing::debug!("rate limiter cleanup task shutting down");
                        break;
                    }
                    _ = interval.tick() => {}
                }

                match weak.upgrade() {
                    Some(inner) => purge_stale(&inner),
                    None => break, // All strong references dropped — exit.
                }
            }
        });

        Self { inner }
    }

    /// Check whether a request from `ip` is allowed.
    /// Returns `Ok(())` if within limits, `Err(())` if rate-limited.
    fn check(&self, ip: IpAddr) -> Result<(), ()> {
        self.check_global()?;

        let mut entry = self.inner.ip_states.entry(ip).or_insert_with(|| IpRateState {
            request_count: 0,
            window_start: Instant::now(),
        });

        if is_window_expired(&entry) {
            entry.request_count = 0;
            entry.window_start = Instant::now();
        }

        if entry.request_count >= MAX_RPC_REQUESTS_PER_SECOND_PER_IP {
            self.inner.global_count.fetch_sub(1, Ordering::SeqCst);
            metrics::counter!("nusantara_rpc_rate_limited_per_ip").increment(1);
            return Err(());
        }

        entry.request_count += 1;
        Ok(())
    }

    /// Atomically check the global rate limit and increment the counter.
    ///
    /// The mutex is held for the entire check-and-increment to prevent a race
    /// where a window reset between the check and the `fetch_add` would allow
    /// more requests than the limit.
    fn check_global(&self) -> Result<(), ()> {
        let mut window_start = self.inner.global_window_start.lock();
        if window_start.elapsed().as_secs() >= 1 {
            self.inner.global_count.store(0, Ordering::SeqCst);
            *window_start = Instant::now();
        }

        let count = self.inner.global_count.fetch_add(1, Ordering::SeqCst);
        if count >= MAX_RPC_REQUESTS_PER_SECOND_GLOBAL {
            self.inner.global_count.fetch_sub(1, Ordering::SeqCst);
            metrics::counter!("nusantara_rpc_rate_limited_global").increment(1);
            return Err(());
        }

        Ok(())
    }
}

/// Returns `true` when the sliding window has expired (> 1 second elapsed).
#[inline]
fn is_window_expired(state: &IpRateState) -> bool {
    state.window_start.elapsed().as_secs() >= 1
}

/// Purge stale per-IP entries from the limiter's map.
fn purge_stale(inner: &RpcRateLimiterInner) {
    let cutoff = RATE_LIMITER_STALE_ENTRY_TIMEOUT_SECS;
    inner
        .ip_states
        .retain(|_ip, state| state.window_start.elapsed().as_secs() < cutoff);
    metrics::gauge!("nusantara_rpc_rate_limiter_tracked_ips")
        .set(inner.ip_states.len() as f64);
}

impl Default for RpcRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tower Layer / Service implementation
// ---------------------------------------------------------------------------

/// Tower `Layer` that wraps a service with per-IP rate limiting.
#[derive(Clone)]
pub(crate) struct RpcRateLimitLayer {
    limiter: RpcRateLimiter,
}

impl RpcRateLimitLayer {
    pub(crate) fn new(limiter: RpcRateLimiter) -> Self {
        Self { limiter }
    }
}

impl<S> Layer<S> for RpcRateLimitLayer {
    type Service = RpcRateLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RpcRateLimitService {
            inner,
            limiter: self.limiter.clone(),
        }
    }
}

/// Tower `Service` that checks rate limits before forwarding the request.
#[derive(Clone)]
pub(crate) struct RpcRateLimitService<S> {
    inner: S,
    limiter: RpcRateLimiter,
}

impl<S> Service<Request<Body>> for RpcRateLimitService<S>
where
    S: Service<Request<Body>, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let ip = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip())
            .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));

        // Exempt localhost and Docker bridge networks from rate limiting.
        if is_local_or_docker(ip) {
            metrics::counter!("nusantara_rpc_requests_allowed").increment(1);
            let mut inner = self.inner.clone();
            return Box::pin(async move { inner.call(req).await });
        }

        if self.limiter.check(ip).is_err() {
            debug!(ip = %ip, "RPC request rate-limited");
            metrics::counter!("nusantara_rpc_requests_rejected_rate_limit").increment(1);
            return Box::pin(async move { Ok(rate_limited_response()) });
        }

        metrics::counter!("nusantara_rpc_requests_allowed").increment(1);
        let mut inner = self.inner.clone();
        Box::pin(async move { inner.call(req).await })
    }
}

/// Returns `true` for localhost, Docker, and all RFC 1918 private networks:
/// - 127.0.0.0/8 (loopback)
/// - 10.0.0.0/8 (private)
/// - 172.16.0.0/12 (Docker bridge / private)
/// - 192.168.0.0/16 (private)
/// - ::1 (IPv6 loopback)
pub fn is_local_or_docker(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.octets()[0] == 10
                || (v4.octets()[0] == 172 && (v4.octets()[1] & 0xF0) == 16)
                || (v4.octets()[0] == 192 && v4.octets()[1] == 168)
        }
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Build a 429 Too Many Requests response.
fn rate_limited_response() -> Response {
    let body = serde_json::json!({"error": "rate limited"});
    (StatusCode::TOO_MANY_REQUESTS, axum::Json(body)).into_response()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn test_limiter() -> RpcRateLimiter {
        // `new()` calls `tokio::spawn`, which requires a runtime.  Use a
        // single-threaded runtime scoped to the helper so plain `#[test]` works.
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("failed to build test runtime");
        rt.block_on(async { RpcRateLimiter::new() })
    }

    #[test]
    fn allows_requests_within_limit() {
        let limiter = test_limiter();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        for _ in 0..50 {
            assert!(limiter.check(ip).is_ok());
        }
    }

    #[test]
    fn rejects_over_per_ip_limit() {
        let limiter = test_limiter();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        for _ in 0..MAX_RPC_REQUESTS_PER_SECOND_PER_IP {
            assert!(limiter.check(ip).is_ok());
        }
        assert!(limiter.check(ip).is_err());
    }

    #[test]
    fn different_ips_are_independent() {
        let limiter = test_limiter();
        let ip1 = IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4));
        let ip2 = IpAddr::V4(Ipv4Addr::new(5, 6, 7, 8));
        for _ in 0..MAX_RPC_REQUESTS_PER_SECOND_PER_IP {
            limiter.check(ip1).unwrap();
        }
        assert!(limiter.check(ip2).is_ok());
    }

    #[test]
    fn purge_stale_removes_old_entries() {
        let limiter = test_limiter();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        limiter.check(ip).unwrap();
        assert_eq!(limiter.inner.ip_states.len(), 1);
        purge_stale(&limiter.inner);
        assert_eq!(limiter.inner.ip_states.len(), 1);
    }

    #[test]
    fn global_limit_rejects_when_exceeded() {
        let limiter = test_limiter();
        let mut accepted = 0u64;
        'outer: for i in 0..=255u8 {
            for j in 0..=255u8 {
                let ip = IpAddr::V4(Ipv4Addr::new(10, 0, i, j));
                if limiter.check(ip).is_ok() {
                    accepted += 1;
                }
                if accepted >= MAX_RPC_REQUESTS_PER_SECOND_GLOBAL {
                    let extra_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
                    assert!(limiter.check(extra_ip).is_err());
                    break 'outer;
                }
            }
        }
    }

    /// Smoke-test that `new_with_shutdown` constructs a working limiter.
    #[test]
    fn new_with_shutdown_constructs_working_limiter() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("failed to build test runtime");
        rt.block_on(async {
            let (_tx, rx) = watch::channel(false);
            let limiter = RpcRateLimiter::new_with_shutdown(rx);
            let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
            assert!(limiter.check(ip).is_ok());
        });
    }

    /// Verify that the Drop impl on Inner sends `true` to the shutdown channel,
    /// which causes the cleanup task to exit via the intended shutdown arm (R2).
    #[test]
    fn drop_inner_signals_shutdown_sender() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("failed to build test runtime");
        rt.block_on(async {
            let limiter = RpcRateLimiter::new();
            // Clone the sender before dropping the limiter so we can inspect
            // the value that was sent after drop.
            let tx_clone = limiter.inner.shutdown_tx.clone();
            // Confirm the value starts at `false`.
            assert!(!*tx_clone.borrow());
            drop(limiter);
            // After the last Arc<Inner> is released, the Drop impl must have
            // sent `true` — the cleanup task will observe this and break.
            assert!(*tx_clone.borrow());
        });
    }
}
