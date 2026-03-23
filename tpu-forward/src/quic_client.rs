use std::net::SocketAddr;
use std::sync::Arc;

use nusantara_core::transaction::Transaction;
use quinn::Endpoint;
use tracing::{debug, instrument};

use crate::connection_cache::ConnectionCache;
use crate::error::TpuError;
use crate::protocol::TpuMessage;

pub struct TpuQuicClient {
    endpoint: Endpoint,
    cache: Arc<ConnectionCache>,
}

impl TpuQuicClient {
    pub fn new(endpoint: Endpoint, cache: Arc<ConnectionCache>) -> Self {
        Self { endpoint, cache }
    }

    /// Send a single transaction to the specified address.
    #[instrument(skip(self, tx), fields(%addr))]
    pub async fn send_transaction(
        &self,
        addr: SocketAddr,
        tx: &Transaction,
    ) -> Result<(), TpuError> {
        let msg = TpuMessage::Transaction(Box::new(tx.clone()));
        self.send_message(addr, &msg).await
    }

    /// Send a batch of transactions to the specified address.
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

        let bytes = msg
            .serialize_to_bytes()
            .map_err(TpuError::Serialization)?;

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

    async fn get_or_connect(
        &self,
        addr: SocketAddr,
    ) -> Result<Arc<quinn::Connection>, TpuError> {
        // Check cache first
        if let Some(conn) = self.cache.get(&addr) {
            if conn.close_reason().is_none() {
                return Ok(conn);
            }
            self.cache.remove(&addr);
        }

        // Create new connection
        let conn = self
            .endpoint
            .connect(addr, "nusantara")
            .map_err(|e| TpuError::QuicConnection(e.to_string()))?
            .await
            .map_err(|e| TpuError::QuicConnection(e.to_string()))?;

        let conn = Arc::new(conn);
        self.cache.insert(addr, Arc::clone(&conn));

        debug!(%addr, "new QUIC connection established");
        Ok(conn)
    }
}
