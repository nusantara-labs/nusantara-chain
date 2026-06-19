use std::net::SocketAddr;
use std::sync::Arc;

use dashmap::DashMap;
use nusantara_core::transaction::Transaction;
use quinn::Endpoint;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tracing::{debug, instrument};

use crate::connection_cache::ConnectionCache;
use crate::error::TpuError;
use crate::protocol::TpuMessage;

/// RAII guard that removes the per-addr inflight entry on drop.
///
/// Ensures cleanup on every exit path from `get_or_connect`: normal return,
/// error return via `?`, and future cancellation mid-connect.
struct InflightGuard<'a> {
    map: &'a DashMap<SocketAddr, Arc<Mutex<()>>>,
    addr: SocketAddr,
    /// When true, drop removes the entry. Call `disarm()` before the explicit
    /// `inflight.remove()` call so the removal is not duplicated.
    active: bool,
}

impl<'a> InflightGuard<'a> {
    fn new(map: &'a DashMap<SocketAddr, Arc<Mutex<()>>>, addr: SocketAddr) -> Self {
        Self {
            map,
            addr,
            active: true,
        }
    }

    /// Disarm so drop is a no-op. Call immediately before the explicit remove.
    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for InflightGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            self.map.remove(&self.addr);
        }
    }
}

pub struct TpuQuicClient {
    endpoint: Endpoint,
    cache: Arc<ConnectionCache>,
    /// One mutex per destination address serialises concurrent connect attempts
    /// to the same peer, preventing duplicate-connect races without blocking
    /// connects to different peers.
    inflight: DashMap<SocketAddr, Arc<Mutex<()>>>,
}

impl TpuQuicClient {
    pub fn new(endpoint: Endpoint, cache: Arc<ConnectionCache>) -> Self {
        Self {
            endpoint,
            cache,
            inflight: DashMap::new(),
        }
    }

    /// Send a single transaction to `addr`.
    #[instrument(skip(self, tx), fields(%addr))]
    pub async fn send_transaction(
        &self,
        addr: SocketAddr,
        tx: Transaction,
    ) -> Result<(), TpuError> {
        let msg = TpuMessage::Transaction(Box::new(tx));
        self.send_message(addr, &msg).await
    }

    /// Send a batch of transactions to `addr`.
    ///
    /// Takes ownership of `txs` — no clone required at the call site.
    #[instrument(skip(self, txs), fields(%addr, batch_size = txs.len()))]
    pub async fn send_batch(
        &self,
        addr: SocketAddr,
        txs: Vec<Transaction>,
    ) -> Result<(), TpuError> {
        let msg = TpuMessage::TransactionBatch(txs);
        self.send_message(addr, &msg).await
    }

    async fn send_message(&self, addr: SocketAddr, msg: &TpuMessage) -> Result<(), TpuError> {
        let conn = self.get_or_connect(addr).await?;

        let mut stream = conn
            .open_uni()
            .await
            .map_err(|e| TpuError::QuicStream(e.to_string()))?;

        let bytes = msg.serialize_to_bytes()?;

        stream
            .write_all(&bytes)
            .await
            .map_err(|e| TpuError::QuicStream(e.to_string()))?;

        stream
            .finish()
            .map_err(|e| TpuError::QuicStream(e.to_string()))?;

        metrics::counter!("nusantara_tpu_forward_messages_sent_total").increment(1);
        Ok(())
    }

    /// Get an existing healthy connection or establish a new one, with dedup.
    ///
    /// If two callers race to connect to the same addr, only one does the actual
    /// `endpoint.connect` — the other waits on the per-addr mutex and then finds
    /// the connection already in cache.  On every exit path (success, error, or
    /// future cancellation) the inflight entry is cleaned up via `InflightGuard`.
    async fn get_or_connect(&self, addr: SocketAddr) -> Result<Arc<quinn::Connection>, TpuError> {
        // Fast path: healthy connection in cache.
        if let Some(conn) = self.cache.get(&addr) {
            if conn.close_reason().is_none() {
                return Ok(conn);
            }
            self.cache.remove(&addr);
        }

        // Acquire per-addr mutex to serialise concurrent connect attempts.
        // The InflightGuard is armed immediately after the entry() insert,
        // before any await, so cancellation at lock_owned().await still
        // triggers cleanup — the invariant is locally obvious: guard is
        // constructed on the very next line after the entry is live in the map.
        let lock: Arc<Mutex<()>> = self
            .inflight
            .entry(addr)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();

        // Guard armed immediately after entry is inserted — no await in between.
        // Ensures the inflight entry is removed on every exit path (success,
        // error via `?`, or future cancellation mid lock_owned().await).
        let mut inflight_guard = InflightGuard::new(&self.inflight, addr);

        let _guard: OwnedMutexGuard<()> = lock.lock_owned().await;

        // Re-check cache: another waiter may have inserted while we blocked.
        if let Some(conn) = self.cache.get(&addr) {
            if conn.close_reason().is_none() {
                return Ok(conn);
            }
            self.cache.remove(&addr);
        }

        // We hold the per-addr lock — safe to connect without racing.
        // If connect() or .await returns Err, `?` propagates and the
        // InflightGuard drop removes the inflight entry.
        let conn = self
            .endpoint
            .connect(addr, "nusantara")
            .map_err(|e| TpuError::QuicConnection(e.to_string()))?
            .await
            .map_err(|e| TpuError::QuicConnection(e.to_string()))?;

        let conn = Arc::new(conn);
        self.cache.insert(addr, Arc::clone(&conn));

        // Disarm the guard so its drop is a no-op, then remove explicitly.
        // The Arc keeps the Mutex alive until all waiting tasks have moved on.
        inflight_guard.disarm();
        self.inflight.remove(&addr);

        debug!(%addr, "new QUIC connection established");
        Ok(conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that `InflightGuard` removes the entry from the map on drop
    /// without requiring `disarm()`.  Simulates future cancellation: construct
    /// the guard, then drop it while `active = true`.
    #[test]
    fn inflight_guard_cleans_up_on_drop() {
        let map: DashMap<SocketAddr, Arc<Mutex<()>>> = DashMap::new();
        let addr: SocketAddr = "127.0.0.1:9000".parse().unwrap();

        // Insert the entry as `get_or_connect` would.
        map.entry(addr).or_insert_with(|| Arc::new(Mutex::new(())));

        assert!(map.contains_key(&addr));

        // Arm the guard (active = true).
        let guard = InflightGuard::new(&map, addr);

        // Drop without disarming — simulates cancellation.
        drop(guard);

        // The entry must have been removed.
        assert!(
            !map.contains_key(&addr),
            "InflightGuard did not remove inflight entry on drop"
        );
    }

    /// Verify that a disarmed `InflightGuard` leaves the entry intact so the
    /// caller's explicit `inflight.remove()` is the sole removal.
    #[test]
    fn inflight_guard_disarmed_does_not_remove() {
        let map: DashMap<SocketAddr, Arc<Mutex<()>>> = DashMap::new();
        let addr: SocketAddr = "127.0.0.1:9001".parse().unwrap();

        map.entry(addr).or_insert_with(|| Arc::new(Mutex::new(())));

        let mut guard = InflightGuard::new(&map, addr);
        guard.disarm();
        drop(guard);

        // Entry must still be present — caller is responsible for explicit remove.
        assert!(
            map.contains_key(&addr),
            "disarmed InflightGuard incorrectly removed the entry"
        );
    }
}
