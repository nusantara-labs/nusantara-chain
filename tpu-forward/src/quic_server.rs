use std::net::IpAddr;
use std::sync::Arc;

use nusantara_core::native_token::const_parse_u64;
use nusantara_core::transaction::Transaction;
use quinn::Endpoint;
use tokio::sync::{Semaphore, mpsc, watch};
use tokio::time::timeout;
use tracing::{debug, info, instrument, warn};

use crate::protocol::TpuMessage;
use crate::rate_limiter::RateLimiter;
use crate::tx_validator;

/// RAII guard that calls `rate_limiter.remove_connection(ip)` on drop.
///
/// Ensures the connection count is decremented on every exit path from the
/// spawned connection task: normal completion, error return, and task
/// cancellation.  Without this, a panic or cancellation before the explicit
/// `remove_connection` call would leak the slot permanently.
struct ConnectionGuard {
    rate_limiter: Arc<RateLimiter>,
    ip: IpAddr,
}

impl ConnectionGuard {
    fn new(rate_limiter: Arc<RateLimiter>, ip: IpAddr) -> Self {
        Self { rate_limiter, ip }
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.rate_limiter.remove_connection(self.ip);
    }
}

pub const MAX_CONCURRENT_CONNECTIONS: u64 =
    const_parse_u64(env!("NUSA_TPU_MAX_CONCURRENT_CONNECTIONS"));

/// Per-stream read deadline in milliseconds.
const STREAM_TIMEOUT_MS: u64 = const_parse_u64(env!("NUSA_TPU_STREAM_TIMEOUT_MS"));

pub struct TpuQuicServer {
    endpoint: Endpoint,
    rate_limiter: Arc<RateLimiter>,
    connection_semaphore: Arc<Semaphore>,
}

impl TpuQuicServer {
    pub fn new(endpoint: Endpoint, rate_limiter: Arc<RateLimiter>) -> Self {
        Self {
            endpoint,
            rate_limiter,
            connection_semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS as usize)),
        }
    }

    /// Run the QUIC server, accepting connections and forwarding valid transactions.
    #[instrument(skip_all, name = "nusantara_tpu_quic_server")]
    pub async fn run(
        self,
        tx_sender: mpsc::Sender<Transaction>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        info!(
            addr = %self.endpoint.local_addr()
                .map_or_else(|_| "unknown".to_string(), |a| a.to_string()),
            "TPU QUIC server started"
        );

        loop {
            tokio::select! {
                biased;
                incoming = self.endpoint.accept() => {
                    let Some(incoming) = incoming else {
                        info!("QUIC endpoint closed");
                        break;
                    };

                    let remote = incoming.remote_address();
                    let ip = remote.ip();

                    // Atomic check-and-add connection (eliminates TOCTOU).
                    if let Err(e) = self.rate_limiter.try_add_connection(ip) {
                        debug!(%remote, error = %e, "connection rejected (per-IP limit)");
                        continue;
                    }

                    // Bound concurrent connection tasks.
                    let permit = match self.connection_semaphore.clone().try_acquire_owned() {
                        Ok(permit) => permit,
                        Err(_) => {
                            self.rate_limiter.remove_connection(ip);
                            metrics::counter!("nusantara_tpu_connections_rejected_total").increment(1);
                            debug!(%remote, "connection rejected (semaphore full)");
                            continue;
                        }
                    };

                    let rate_limiter = Arc::clone(&self.rate_limiter);
                    let tx_sender = tx_sender.clone();

                    tokio::spawn(async move {
                        let _permit = permit; // held until task completes
                        // Drop guard created BEFORE any await — removes the
                        // connection count on every exit path including cancellation.
                        let _conn_guard = ConnectionGuard::new(Arc::clone(&rate_limiter), ip);
                        match incoming.await {
                            Ok(conn) => {
                                handle_connection(conn, ip, &rate_limiter, &tx_sender).await;
                            }
                            Err(e) => {
                                debug!(%remote, error = %e, "incoming connection failed");
                            }
                        }
                        // _conn_guard dropped here — remove_connection called.
                    });
                }
                _ = shutdown.changed() => {
                    break;
                }
            }
        }

        self.endpoint.close(0u32.into(), b"shutdown");
        info!("TPU QUIC server stopped");
    }
}

async fn handle_connection(
    conn: quinn::Connection,
    ip: std::net::IpAddr,
    rate_limiter: &RateLimiter,
    tx_sender: &mpsc::Sender<Transaction>,
) {
    let stream_deadline = tokio::time::Duration::from_millis(STREAM_TIMEOUT_MS);

    loop {
        // Apply a timeout to accept_uni so a slow/idle connection does not hold
        // a connection slot indefinitely.
        let mut stream_result = match timeout(stream_deadline, conn.accept_uni()).await {
            Ok(Ok(s)) => s,
            Ok(Err(quinn::ConnectionError::ApplicationClosed(_))) => break,
            Ok(Err(e)) => {
                debug!(%ip, error = %e, "stream accept error");
                break;
            }
            Err(_elapsed) => {
                debug!(%ip, "accept_uni timed out — freeing connection slot");
                break;
            }
        };

        // Bound the read with the same timeout — prevents slow-loris on the read side.
        let read_result = timeout(
            stream_deadline,
            stream_result.read_to_end(crate::tx_validator::MAX_BATCH_WIRE_SIZE),
        )
        .await;

        let data = match read_result {
            Ok(Ok(d)) => d,
            Ok(Err(e)) => {
                debug!(%ip, error = %e, "stream read error");
                continue;
            }
            Err(_elapsed) => {
                debug!(%ip, "stream read timed out");
                continue;
            }
        };

        // deserialize_from_bytes returns the exact decompressed byte length alongside
        // the decoded message, eliminating the need to estimate from the wire size.
        match TpuMessage::deserialize_from_bytes(&data) {
            Ok((TpuMessage::SignedBatch(batch), decompressed_len)) => {
                // validate_batch: cheap structural → absolute size cap → entry count →
                // per-entry structural → 1 Dilithium3 → N Merkle.
                // We pass the exact decompressed length — not an estimate from the
                // compressed wire size, which is always wrong on at least one side.
                if let Err(e) = tx_validator::validate_batch(&batch, decompressed_len) {
                    debug!(%ip, error = %e, "invalid batch");
                    metrics::counter!("nusantara_tpu_invalid_transactions_total").increment(1);
                    continue;
                }

                let n = batch.entries.len() as u64;

                // Rate-limit the whole batch as N tokens in one atomic call.
                if let Err(e) = rate_limiter.check_rate_limit_n(ip, n) {
                    debug!(%ip, error = %e, "rate limited (batch)");
                    return;
                }

                metrics::counter!("nusantara_tpu_batches_received_total").increment(1);
                // Increment per-tx counter only after successful rate-limit.
                metrics::counter!("nusantara_tpu_transactions_received_total").increment(n);

                for tx in batch.to_transactions() {
                    if tx_sender.send(tx).await.is_err() {
                        warn!("tx channel closed");
                        return;
                    }
                }
            }
            Ok((msg, decompressed_len)) => {
                // Single Transaction or unsigned TransactionBatch.
                // into_transactions() avoids cloning — consumes the message.
                let txs = msg.into_transactions();

                for tx in txs {
                    // Rate-limit each tx individually.
                    if let Err(e) = rate_limiter.check_rate_limit(ip) {
                        debug!(%ip, error = %e, "rate limited");
                        return;
                    }

                    // Use the full decompressed message length as a conservative
                    // upper bound on each transaction's serialized size.  For a
                    // single-tx message this is exact (minus the 1-byte enum tag);
                    // for a batch it is a safe ceiling because no individual tx can
                    // be larger than the total message.  This avoids a per-tx
                    // `borsh::to_vec` re-serialization (O(N) allocations per batch)
                    // while still closing the compression bypass: decompressed_len
                    // is measured after decompression, not from the wire size.
                    if let Err(e) = tx_validator::validate(&tx, decompressed_len) {
                        debug!(%ip, error = %e, "invalid transaction");
                        metrics::counter!("nusantara_tpu_invalid_transactions_total").increment(1);
                        continue;
                    }

                    // Increment only after successful rate-limit + validate.
                    metrics::counter!("nusantara_tpu_transactions_received_total").increment(1);

                    if tx_sender.send(tx).await.is_err() {
                        warn!("tx channel closed");
                        return;
                    }
                }
            }
            Err(e) => {
                debug!(%ip, error = %e, "failed to deserialize TPU message");
            }
        }
    }
}
