use std::net::SocketAddr;
use std::sync::Arc;

use dashmap::DashMap;
use quinn::Connection;

/// Maximum number of cached QUIC connections to prevent unbounded growth.
pub const MAX_CACHED_CONNECTIONS: usize = 256;

pub struct ConnectionCache {
    connections: DashMap<SocketAddr, Arc<Connection>>,
}

impl ConnectionCache {
    pub fn new() -> Self {
        Self {
            connections: DashMap::new(),
        }
    }

    /// Get a cached connection or return None.
    pub fn get(&self, addr: &SocketAddr) -> Option<Arc<Connection>> {
        self.connections.get(addr).map(|c| Arc::clone(c.value()))
    }

    /// Cache a connection. If at capacity, prune closed connections first,
    /// then evict an arbitrary entry if still full.
    pub fn insert(&self, addr: SocketAddr, conn: Arc<Connection>) {
        if self.connections.len() >= MAX_CACHED_CONNECTIONS {
            self.prune_closed();
        }
        if self.connections.len() >= MAX_CACHED_CONNECTIONS {
            // Evict an arbitrary entry to make room
            if let Some(entry) = self.connections.iter().next() {
                let evict_addr = *entry.key();
                drop(entry);
                self.connections.remove(&evict_addr);
            }
        }
        self.connections.insert(addr, conn);
    }

    /// Remove a connection (e.g. on error).
    pub fn remove(&self, addr: &SocketAddr) {
        self.connections.remove(addr);
    }

    /// Remove stale connections.
    pub fn prune_closed(&self) {
        self.connections.retain(|_, conn| {
            conn.close_reason().is_none()
        });
    }

    pub fn len(&self) -> usize {
        self.connections.len()
    }

    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
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

    // ConnectionCache basic tests (no real QUIC connections in unit tests)

    #[test]
    fn empty_cache() {
        let cache = ConnectionCache::new();
        let addr: SocketAddr = "127.0.0.1:8000".parse().unwrap();
        assert!(cache.get(&addr).is_none());
        assert!(cache.is_empty());
    }
}
