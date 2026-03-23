use std::net::SocketAddr;
use std::sync::Arc;

use nusantara_core::native_token::const_parse_u64;
use nusantara_core::transaction::Transaction;
use nusantara_crypto::Hash;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, instrument, warn};

use crate::quic_client::TpuQuicClient;

pub const FORWARD_BATCH_SIZE: u64 = const_parse_u64(env!("NUSA_TPU_FORWARD_BATCH_SIZE"));
pub const FORWARD_INTERVAL_MS: u64 = const_parse_u64(env!("NUSA_TPU_FORWARD_INTERVAL_MS"));

pub struct TransactionForwarder {
    my_identity: Hash,
    client: Arc<TpuQuicClient>,
}

impl TransactionForwarder {
    pub fn new(my_identity: Hash, client: Arc<TpuQuicClient>) -> Self {
        Self {
            my_identity,
            client,
        }
    }

    /// Forward transactions to the current leader.
    /// `leader_lookup` returns (leader_identity, leader_tpu_addr) for the current slot.
    /// `local_tx_sender` is used when we ARE the leader.
    #[instrument(skip_all, name = "forwarder")]
    pub async fn run<F>(
        self,
        mut tx_receiver: mpsc::Receiver<Transaction>,
        local_tx_sender: mpsc::Sender<Transaction>,
        leader_lookup: F,
        mut shutdown: watch::Receiver<bool>,
    ) where
        F: Fn() -> Option<(Hash, SocketAddr)>,
    {
        let interval = tokio::time::Duration::from_millis(FORWARD_INTERVAL_MS);
        let mut tick = tokio::time::interval(interval);
        let mut batch = Vec::with_capacity(FORWARD_BATCH_SIZE as usize);

        loop {
            tokio::select! {
                biased;
                Some(tx) = tx_receiver.recv() => {
                    batch.push(tx);

                    if batch.len() >= FORWARD_BATCH_SIZE as usize {
                        self.flush_batch(&mut batch, &leader_lookup, &local_tx_sender).await;
                    }
                }
                _ = tick.tick() => {
                    if !batch.is_empty() {
                        self.flush_batch(&mut batch, &leader_lookup, &local_tx_sender).await;
                    }
                }
                _ = shutdown.changed() => {
                    // Flush remaining
                    if !batch.is_empty() {
                        self.flush_batch(&mut batch, &leader_lookup, &local_tx_sender).await;
                    }
                    break;
                }
            }
        }

        info!("transaction forwarder stopped");
    }

    async fn flush_batch<F>(
        &self,
        batch: &mut Vec<Transaction>,
        leader_lookup: &F,
        local_tx_sender: &mpsc::Sender<Transaction>,
    ) where
        F: Fn() -> Option<(Hash, SocketAddr)>,
    {
        let txs: Vec<Transaction> = std::mem::take(batch);

        match leader_lookup() {
            Some((leader_id, leader_addr)) => {
                if leader_id == self.my_identity {
                    // We are the leader — send to local block producer
                    for tx in txs {
                        if local_tx_sender.send(tx).await.is_err() {
                            warn!("local tx channel closed");
                            return;
                        }
                    }
                    metrics::counter!("nusantara_tpu_local_forward_total").increment(1);
                } else {
                    // Forward to remote leader
                    if let Err(e) = self.client.send_batch(leader_addr, txs).await {
                        debug!(error = %e, "failed to forward batch to leader");
                        metrics::counter!("nusantara_tpu_forward_errors_total").increment(1);
                    } else {
                        metrics::counter!("nusantara_tpu_remote_forward_total").increment(1);
                    }
                }
            }
            None => {
                debug!("no leader available, dropping {} transactions", txs.len());
                metrics::counter!("nusantara_tpu_no_leader_drops_total").increment(txs.len() as u64);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_values() {
        assert_eq!(FORWARD_BATCH_SIZE, 64);
        assert_eq!(FORWARD_INTERVAL_MS, 10);
    }
}
