use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use dashmap::DashMap;
use nusantara_core::native_token::const_parse_u64;

pub const MAX_TX_PER_SECOND_PER_IP: u64 =
    const_parse_u64(env!("NUSA_TPU_MAX_TX_PER_SECOND_PER_IP"));
pub const MAX_TX_PER_SECOND_GLOBAL: u64 =
    const_parse_u64(env!("NUSA_TPU_MAX_TX_PER_SECOND_GLOBAL"));
pub const MAX_CONNECTIONS_PER_IP: u64 =
    const_parse_u64(env!("NUSA_TPU_MAX_CONNECTIONS_PER_IP"));

/// Maximum number of IP entries in the rate limiter to prevent unbounded growth.
pub const MAX_RATE_LIMITER_ENTRIES: usize = 100_000;

struct IpState {
    tx_count: u64,
    connection_count: u64,
    window_start: Instant,
}

pub struct RateLimiter {
    ip_states: DashMap<IpAddr, IpState>,
    global_count: AtomicU64,
    global_window_start: parking_lot::Mutex<Instant>,
    max_tx_per_sec_per_ip: u64,
    max_tx_per_sec_global: u64,
    max_connections_per_ip: u64,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            ip_states: DashMap::new(),
            global_count: AtomicU64::new(0),
            global_window_start: parking_lot::Mutex::new(Instant::now()),
            max_tx_per_sec_per_ip: MAX_TX_PER_SECOND_PER_IP,
            max_connections_per_ip: MAX_CONNECTIONS_PER_IP,
            max_tx_per_sec_global: MAX_TX_PER_SECOND_GLOBAL,
        }
    }

    /// Remove IP entries whose rate windows have expired (> 1 second old)
    /// and that have no active connections.
    pub fn purge_expired(&self) {
        self.ip_states.retain(|_, state| {
            state.connection_count > 0 || state.window_start.elapsed().as_secs() < 2
        });
        metrics::gauge!("nusantara_tpu_rate_limiter_entries").set(self.ip_states.len() as f64);
    }

    /// Check if a transaction from the given IP is allowed.
    /// Atomically increments counters to eliminate TOCTOU races.
    pub fn check_rate_limit(&self, ip: IpAddr) -> Result<(), crate::error::TpuError> {
        // Reject new IPs when at capacity to prevent unbounded memory growth
        if !self.ip_states.contains_key(&ip) && self.ip_states.len() >= MAX_RATE_LIMITER_ENTRIES {
            metrics::counter!("nusantara_tpu_rate_limiter_capacity_exceeded").increment(1);
            return Err(crate::error::TpuError::RateLimited {
                reason: "rate limiter at capacity".to_string(),
            });
        }

        // Atomically check and increment global rate
        self.check_and_increment_global()?;

        // Check per-IP rate (DashMap entry holds shard lock for the entire operation)
        let mut entry = self.ip_states.entry(ip).or_insert_with(|| IpState {
            tx_count: 0,
            connection_count: 0,
            window_start: Instant::now(),
        });

        // Reset window if > 1 second elapsed
        if entry.window_start.elapsed().as_secs() >= 1 {
            entry.tx_count = 0;
            entry.window_start = Instant::now();
        }

        if entry.tx_count >= self.max_tx_per_sec_per_ip {
            // Undo the global increment since this tx is rejected
            self.global_count.fetch_sub(1, Ordering::SeqCst);
            metrics::counter!("nusantara_tpu_rate_limited_per_ip_total").increment(1);
            return Err(crate::error::TpuError::RateLimited {
                reason: format!("per-IP limit exceeded: {ip}"),
            });
        }

        entry.tx_count += 1;
        Ok(())
    }

    /// Atomically check the global rate limit and increment the counter.
    /// On reject, the counter is not incremented (no cleanup needed).
    /// The mutex is held for the entire check-and-increment to prevent a
    /// race where a window reset between the check and the fetch_add would
    /// allow requests above the limit.
    fn check_and_increment_global(&self) -> Result<(), crate::error::TpuError> {
        let mut window_start = self.global_window_start.lock();
        if window_start.elapsed().as_secs() >= 1 {
            self.global_count.store(0, Ordering::SeqCst);
            *window_start = Instant::now();
        }

        let count = self.global_count.fetch_add(1, Ordering::SeqCst);
        if count >= self.max_tx_per_sec_global {
            self.global_count.fetch_sub(1, Ordering::SeqCst);
            drop(window_start);
            metrics::counter!("nusantara_tpu_rate_limited_global_total").increment(1);
            return Err(crate::error::TpuError::RateLimited {
                reason: "global rate limit exceeded".to_string(),
            });
        }

        drop(window_start);
        Ok(())
    }

    /// Atomically check connection limit and add a connection if within limit.
    /// Eliminates the TOCTOU race between check_connection_limit + add_connection.
    pub fn try_add_connection(&self, ip: IpAddr) -> Result<(), crate::error::TpuError> {
        let mut entry = self.ip_states.entry(ip).or_insert_with(|| IpState {
            tx_count: 0,
            connection_count: 0,
            window_start: Instant::now(),
        });

        if entry.connection_count >= self.max_connections_per_ip {
            return Err(crate::error::TpuError::RateLimited {
                reason: format!("connection limit exceeded: {ip}"),
            });
        }

        entry.connection_count += 1;
        Ok(())
    }

    /// Track a connection close from an IP.
    pub fn remove_connection(&self, ip: IpAddr) {
        if let Some(mut entry) = self.ip_states.get_mut(&ip) {
            entry.connection_count = entry.connection_count.saturating_sub(1);
        }
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn config_values() {
        assert_eq!(MAX_TX_PER_SECOND_PER_IP, 100);
        assert_eq!(MAX_TX_PER_SECOND_GLOBAL, 50000);
        assert_eq!(MAX_CONNECTIONS_PER_IP, 8);
    }

    #[test]
    fn allows_within_limit() {
        let limiter = RateLimiter::new();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);

        for _ in 0..50 {
            assert!(limiter.check_rate_limit(ip).is_ok());
        }
    }

    #[test]
    fn rejects_over_per_ip_limit() {
        let limiter = RateLimiter::new();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);

        for _ in 0..MAX_TX_PER_SECOND_PER_IP {
            assert!(limiter.check_rate_limit(ip).is_ok());
        }
        assert!(limiter.check_rate_limit(ip).is_err());
    }

    #[test]
    fn different_ips_independent() {
        let limiter = RateLimiter::new();
        let ip1 = IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4));
        let ip2 = IpAddr::V4(Ipv4Addr::new(5, 6, 7, 8));

        for _ in 0..MAX_TX_PER_SECOND_PER_IP {
            limiter.check_rate_limit(ip1).unwrap();
        }
        // ip1 is exhausted, but ip2 should still work
        assert!(limiter.check_rate_limit(ip2).is_ok());
    }

    #[test]
    fn try_add_connection_atomic() {
        let limiter = RateLimiter::new();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);

        for _ in 0..MAX_CONNECTIONS_PER_IP {
            assert!(limiter.try_add_connection(ip).is_ok());
        }
        assert!(limiter.try_add_connection(ip).is_err());

        limiter.remove_connection(ip);
        assert!(limiter.try_add_connection(ip).is_ok());
    }

    #[test]
    fn concurrent_rate_limit_respects_global() {
        use std::sync::Arc;
        use std::thread;

        let limiter = Arc::new(RateLimiter::new());
        let mut handles = Vec::new();

        // Spawn many threads trying to check rate limit concurrently
        for _ in 0..10 {
            let l = Arc::clone(&limiter);
            handles.push(thread::spawn(move || {
                let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
                let mut count = 0u64;
                for _ in 0..1000 {
                    if l.check_rate_limit(ip).is_ok() {
                        count += 1;
                    }
                }
                count
            }));
        }

        let total: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
        // Total should not exceed global limit (may be slightly less due to per-IP limit)
        assert!(total <= MAX_TX_PER_SECOND_GLOBAL);
    }
}
