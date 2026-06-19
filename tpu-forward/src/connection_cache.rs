use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use quinn::Connection;

/// Maximum number of cached QUIC connections to prevent unbounded growth.
pub const MAX_CACHED_CONNECTIONS: usize = 256;

/// Per-entry wrapper that tracks last-access time for deterministic LRU eviction.
struct CacheEntry {
    conn: Arc<Connection>,
    /// Logical timestamp from `ConnectionCache::clock`; higher = more recently used.
    last_used: u64,
}

pub struct ConnectionCache {
    entries: DashMap<SocketAddr, CacheEntry>,
    /// Monotonically increasing logical clock; incremented on each access/insert.
    clock: AtomicU64,
    /// Atomic entry count to bound the insert capacity check without TOCTOU.
    /// Best-effort: may transiently exceed MAX_CACHED_CONNECTIONS by at most the
    /// number of concurrent insert races; `prune_closed` resets it to the true
    /// map size.  Consistent approach with RateLimiter::entry_count.
    entry_count: AtomicUsize,
}

impl ConnectionCache {
    pub fn new() -> Self {
        Self {
            entries: DashMap::new(),
            clock: AtomicU64::new(0),
            entry_count: AtomicUsize::new(0),
        }
    }

    fn tick(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed)
    }

    /// Get a cached connection, updating its last-used timestamp.
    pub fn get(&self, addr: &SocketAddr) -> Option<Arc<Connection>> {
        let mut entry = self.entries.get_mut(addr)?;
        entry.last_used = self.tick();
        Some(Arc::clone(&entry.conn))
    }

    /// Insert or update a connection.
    ///
    /// Uses a single `entry()` call to eliminate the speculative `fetch_add` +
    /// `contains_key` + `insert` TOCTOU race in the previous implementation.
    /// Capacity enforcement and counter increment happen exclusively in the
    /// `Vacant` arm while the DashMap shard lock is held, so two concurrent
    /// inserts of the same new addr can never both increment `entry_count`.
    ///
    /// On `Occupied`, the existing entry is updated in-place; `entry_count` is
    /// not touched because the map size is unchanged.
    pub fn insert(&self, addr: SocketAddr, conn: Arc<Connection>) {
        match self.entries.entry(addr) {
            Entry::Vacant(slot) => {
                // New entry — enforce capacity before inserting.
                if self.entry_count.load(Ordering::Acquire) >= MAX_CACHED_CONNECTIONS {
                    // Try to free a slot, then re-check.
                    self.prune_closed();
                    if self.entry_count.load(Ordering::Acquire) >= MAX_CACHED_CONNECTIONS {
                        self.evict_lru();
                        // evict_lru already decrements entry_count for the removed entry.
                    }
                }
                slot.insert(CacheEntry {
                    conn,
                    last_used: self.tick(),
                });
                // Increment only after successful insert — no speculative add to roll back.
                self.entry_count.fetch_add(1, Ordering::AcqRel);
            }
            Entry::Occupied(mut occ) => {
                // Update in-place — map size unchanged, counter unaffected.
                occ.get_mut().conn = conn;
                occ.get_mut().last_used = self.tick();
            }
        }
    }

    /// Remove a connection (e.g. on error).
    pub fn remove(&self, addr: &SocketAddr) {
        if self.entries.remove(addr).is_some() {
            self.entry_count.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Remove entries whose underlying QUIC connection has closed.
    pub fn prune_closed(&self) {
        self.entries.retain(|_, e| e.conn.close_reason().is_none());
        // Sync the atomic counter to the true map size after bulk removal.
        let live = self.entries.len();
        self.entry_count.store(live, Ordering::Relaxed);
        metrics::gauge!("nusantara_tpu_connection_cache_size").set(live as f64);
    }

    /// Evict the least-recently-used entry.
    fn evict_lru(&self) {
        // Find the key with the smallest last_used timestamp.
        let victim = self
            .entries
            .iter()
            .min_by_key(|e| e.value().last_used)
            .map(|e| *e.key());

        if let Some(addr) = victim
            && self.entries.remove(&addr).is_some()
        {
            self.entry_count.fetch_sub(1, Ordering::Relaxed);
            metrics::counter!("nusantara_tpu_connection_cache_evictions_total").increment(1);
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for ConnectionCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_cache() {
        let cache = ConnectionCache::new();
        let addr: SocketAddr = "127.0.0.1:8000".parse().unwrap();
        assert!(cache.get(&addr).is_none());
        assert!(cache.is_empty());
    }

    #[test]
    fn len_is_zero_initially() {
        let cache = ConnectionCache::new();
        assert_eq!(cache.len(), 0);
    }

    /// Verify that `entry_count` stays consistent with the true DashMap size
    /// after a sequence of no-op reads on a fresh cache.
    ///
    /// We cannot construct a real `quinn::Connection` in a unit test, so the
    /// actual Entry-API insert invariant (Occupied branch does not increment
    /// counter) is exercised by the concurrent_same_addr test in rate_limiter.rs,
    /// which uses the identical DashMap::entry() pattern.  Here we simply confirm
    /// the zero-insert baseline holds.
    #[test]
    fn entry_count_consistent_with_len_on_fresh_cache() {
        let cache = ConnectionCache::new();

        assert_eq!(
            cache.entry_count.load(Ordering::Relaxed),
            cache.len(),
            "entry_count drifted from true map size on empty cache"
        );
        assert_eq!(cache.len(), 0);
    }
}
