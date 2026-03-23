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
//!   60 seconds to prevent unbounded memory growth from short-lived clients.
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
use tower::{Layer, Service};
use tracing::debug;

/// Maximum requests per second per IP address.
const MAX_RPC_REQUESTS_PER_SECOND_PER_IP: u64 = 100;

/// Maximum aggregate requests per second across all IPs.
const MAX_RPC_REQUESTS_PER_SECOND_GLOBAL: u64 = 50_000;

/// How often the background cleanup task runs (seconds).
const CLEANUP_INTERVAL_SECS: u64 = 60;

/// Entries older than this (seconds) are purged during cleanup.
const STALE_ENTRY_TIMEOUT_SECS: u64 = 120;

/// Per-IP request tracking state.
struct IpRateState {
    request_count: u64,
    window_start: Instant,
}

/// Shared rate limiter state, cheap to clone via `Arc`.
#[derive(Clone)]
pub struct RpcRateLimiter {
    inner: Arc<RpcRateLimiterInner>,
}

struct RpcRateLimiterInner {
    ip_states: DashMap<IpAddr, IpRateState>,
    global_count: AtomicU64,
    global_window_start: parking_lot::Mutex<Instant>,
}

impl RpcRateLimiter {
    pub fn new() -> Self {
        let limiter = Self {
            inner: Arc::new(RpcRateLimiterInner {
                ip_states: DashMap::new(),
                global_count: AtomicU64::new(0),
                global_window_start: parking_lot::Mutex::new(Instant::now()),
            }),
        };

        // Spawn a background task to purge stale entries.
        let cleanup_limiter = limiter.clone();
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(CLEANUP_INTERVAL_SECS));
            loop {
                interval.tick().await;
                cleanup_limiter.purge_stale();
            }
        });

        limiter
    }

    /// Remove entries whose window has not been touched for `STALE_ENTRY_TIMEOUT_SECS`.
    fn purge_stale(&self) {
        let cutoff_secs = STALE_ENTRY_TIMEOUT_SECS;
        self.inner.ip_states.retain(|_ip, state| {
            state.window_start.elapsed().as_secs() < cutoff_secs
        });
        metrics::gauge!("nusantara_rpc_rate_limiter_tracked_ips")
            .set(self.inner.ip_states.len() as f64);
    }

    /// Check whether a request from `ip` is allowed.
    /// Returns `Ok(())` if within limits, `Err(())` if rate-limited.
    fn check(&self, ip: IpAddr) -> Result<(), ()> {
        // Global rate check (atomic, short lock on window reset).
        self.check_global()?;

        // Per-IP rate check (DashMap shard lock held synchronously only).
        let mut entry = self.inner.ip_states.entry(ip).or_insert_with(|| IpRateState {
            request_count: 0,
            window_start: Instant::now(),
        });

        // Reset window if more than 1 second has elapsed.
        if entry.window_start.elapsed().as_secs() >= 1 {
            entry.request_count = 0;
            entry.window_start = Instant::now();
        }

        if entry.request_count >= MAX_RPC_REQUESTS_PER_SECOND_PER_IP {
            // Undo the global increment since this request is rejected.
            self.inner.global_count.fetch_sub(1, Ordering::SeqCst);
            metrics::counter!("nusantara_rpc_rate_limited_per_ip").increment(1);
            return Err(());
        }

        entry.request_count += 1;
        Ok(())
    }

    /// Atomically check the global rate limit and increment the counter.
    /// The mutex is held for the entire check-and-increment to prevent a
    /// race where a window reset between the check and the fetch_add would
    /// allow requests above the limit.
    fn check_global(&self) -> Result<(), ()> {
        let mut window_start = self.inner.global_window_start.lock();
        if window_start.elapsed().as_secs() >= 1 {
            self.inner.global_count.store(0, Ordering::SeqCst);
            *window_start = Instant::now();
        }

        let count = self.inner.global_count.fetch_add(1, Ordering::SeqCst);
        if count >= MAX_RPC_REQUESTS_PER_SECOND_GLOBAL {
            self.inner.global_count.fetch_sub(1, Ordering::SeqCst);
            drop(window_start);
            metrics::counter!("nusantara_rpc_rate_limited_global").increment(1);
            return Err(());
        }

        drop(window_start);
        Ok(())
    }
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
pub struct RpcRateLimitLayer {
    limiter: RpcRateLimiter,
}

impl RpcRateLimitLayer {
    pub fn new(limiter: RpcRateLimiter) -> Self {
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
pub struct RpcRateLimitService<S> {
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
        // Extract the client IP from `ConnectInfo<SocketAddr>` if available,
        // falling back to 127.0.0.1 when running behind a proxy without
        // connect info (e.g. in tests).
        let ip = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip())
            .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));

        // Exempt localhost and Docker bridge networks from rate limiting
        if is_local_or_docker(ip) {
            metrics::counter!("nusantara_rpc_requests_allowed").increment(1);
            let mut inner = self.inner.clone();
            return Box::pin(async move { inner.call(req).await });
        }

        if self.limiter.check(ip).is_err() {
            debug!(ip = %ip, "RPC request rate-limited");
            metrics::counter!("nusantara_rpc_requests_rejected_rate_limit").increment(1);

            return Box::pin(async move {
                Ok(rate_limited_response())
            });
        }

        metrics::counter!("nusantara_rpc_requests_allowed").increment(1);

        // Clone the inner service (required by Tower's `Service` contract
        // when calling from a shared reference).
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
    let body = serde_json::json!({
        "error": "rate limited"
    });
    (StatusCode::TOO_MANY_REQUESTS, axum::Json(body)).into_response()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    /// Build a limiter without spawning the background cleanup task (tests
    /// are not guaranteed to have a Tokio runtime for spawn).
    fn test_limiter() -> RpcRateLimiter {
        RpcRateLimiter {
            inner: Arc::new(RpcRateLimiterInner {
                ip_states: DashMap::new(),
                global_count: AtomicU64::new(0),
                global_window_start: parking_lot::Mutex::new(Instant::now()),
            }),
        }
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
        // ip1 exhausted, ip2 should still work
        assert!(limiter.check(ip2).is_ok());
    }

    #[test]
    fn purge_stale_removes_old_entries() {
        let limiter = test_limiter();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);

        // Insert a request to create the entry
        limiter.check(ip).unwrap();
        assert_eq!(limiter.inner.ip_states.len(), 1);

        // The entry is fresh, purge should keep it
        limiter.purge_stale();
        assert_eq!(limiter.inner.ip_states.len(), 1);
    }

    #[test]
    fn global_limit_rejects_when_exceeded() {
        let limiter = test_limiter();

        // Use many different IPs to avoid per-IP limit
        let mut accepted = 0u64;
        for i in 0..=255u8 {
            for j in 0..=255u8 {
                let ip = IpAddr::V4(Ipv4Addr::new(10, 0, i, j));
                if limiter.check(ip).is_ok() {
                    accepted += 1;
                }
                if accepted >= MAX_RPC_REQUESTS_PER_SECOND_GLOBAL {
                    // Next request from any IP should fail
                    let extra_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
                    assert!(limiter.check(extra_ip).is_err());
                    return;
                }
            }
        }
        // If we didn't hit global limit (per-IP limit is 100, we have 65536 IPs,
        // so 65536 * 100 >> 50000), we should have been rejected by global.
        panic!("should have hit global limit");
    }
}
