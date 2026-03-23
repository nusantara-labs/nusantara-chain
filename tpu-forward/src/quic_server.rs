use std::sync::Arc;

use nusantara_core::native_token::const_parse_u64;
use nusantara_core::transaction::Transaction;
use quinn::Endpoint;
use tokio::sync::{Semaphore, mpsc, watch};
use tracing::{debug, info, instrument, warn};

use crate::protocol::TpuMessage;
use crate::rate_limiter::RateLimiter;
use crate::tx_validator::TxValidator;

pub const MAX_CONCURRENT_CONNECTIONS: u64 =
    const_parse_u64(env!("NUSA_TPU_MAX_CONCURRENT_CONNECTIONS"));

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
    #[instrument(skip(self, tx_sender, shutdown), name = "nusantara_tpu_quic_server")]
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

                    // Atomic check-and-add connection (eliminates TOCTOU)
                    if let Err(e) = self.rate_limiter.try_add_connection(ip) {
                        debug!(%remote, error = %e, "connection rejected (per-IP limit)");
                        continue;
                    }

                    // Bound concurrent connection tasks
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
                        match incoming.await {
                            Ok(conn) => {
                                handle_connection(conn, ip, &rate_limiter, &tx_sender).await;
                            }
                            Err(e) => {
                                debug!(%remote, error = %e, "incoming connection failed");
                            }
                        }
                        rate_limiter.remove_connection(ip);
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
    loop {
        let mut stream = match conn.accept_uni().await {
            Ok(stream) => stream,
            Err(quinn::ConnectionError::ApplicationClosed(_)) => break,
            Err(e) => {
                debug!(%ip, error = %e, "stream accept error");
                break;
            }
        };

        match stream.read_to_end(crate::tx_validator::MAX_TRANSACTION_SIZE as usize).await {
            Ok(data) => {
                let raw_size = data.len();
                match TpuMessage::deserialize_from_bytes(&data) {
                    Ok(TpuMessage::SignedBatch(batch)) => {
                        // Validate batch (1 Dilithium3 + N Merkle proofs)
                        if let Err(e) = TxValidator::validate_batch(&batch, raw_size) {
                            debug!(%ip, error = %e, "invalid batch");
                            metrics::counter!("nusantara_tpu_invalid_transactions_total").increment(1);
                            continue;
                        }

                        metrics::counter!("nusantara_tpu_batches_received_total").increment(1);
                        metrics::counter!("nusantara_tpu_transactions_received_total")
                            .increment(batch.entries.len() as u64);

                        // Send each entry as a transaction
                        for tx in batch.to_transactions() {
                            if let Err(e) = rate_limiter.check_rate_limit(ip) {
                                debug!(%ip, error = %e, "rate limited");
                                return;
                            }
                            if tx_sender.send(tx).await.is_err() {
                                warn!("tx channel closed");
                                return;
                            }
                        }
                    }
                    Ok(msg) => {
                        for tx in msg.transactions() {
                            if let Err(e) = rate_limiter.check_rate_limit(ip) {
                                debug!(%ip, error = %e, "rate limited");
                                return;
                            }

                            if let Err(e) = TxValidator::validate(&tx, raw_size) {
                                debug!(%ip, error = %e, "invalid transaction");
                                metrics::counter!("nusantara_tpu_invalid_transactions_total").increment(1);
                                continue;
                            }

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
            Err(e) => {
                debug!(%ip, error = %e, "stream read error");
            }
        }
    }
}
