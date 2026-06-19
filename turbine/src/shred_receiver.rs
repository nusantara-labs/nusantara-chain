use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, warn};

use crate::protocol::{MAX_BATCH_RESPONSE_SHREDS, TurbineMessage, MAX_UDP_PACKET};

/// Maximum number of consecutive UDP receive errors before the receiver gives up.
const MAX_CONSECUTIVE_ERRORS: u32 = 100;

pub struct ShredReceiver {
    socket: Arc<UdpSocket>,
}

impl ShredReceiver {
    pub fn new(socket: Arc<UdpSocket>) -> Self {
        Self { socket }
    }

    /// Receive turbine messages from the UDP socket and forward to the retransmit stage.
    /// Passes full `TurbineMessage` (not just shreds) so that headers can flow through.
    ///
    /// Design notes:
    /// - No `biased` in select — round-robin gives shutdown equal priority under flood.
    /// - Consecutive UDP errors trigger backoff and eventual break to avoid tight loops.
    /// - `BatchRepairResponse` shreds are capped at `MAX_BATCH_RESPONSE_SHREDS` to
    ///   prevent a single packet flooding the downstream channel.
    pub async fn run(
        self,
        message_sender: mpsc::Sender<(TurbineMessage, SocketAddr)>,
        repair_sender: mpsc::Sender<(TurbineMessage, SocketAddr)>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let mut buf = vec![0u8; MAX_UDP_PACKET];
        let mut consecutive_errors: u32 = 0;

        loop {
            // Round-robin select (no `biased`) — shutdown gets equal priority
            // so a flooded socket cannot prevent clean shutdown.
            tokio::select! {
                result = self.socket.recv_from(&mut buf) => {
                    match result {
                        Ok((len, src)) => {
                            consecutive_errors = 0;
                            let data = &buf[..len];
                            match TurbineMessage::deserialize_from_bytes(data) {
                                Ok(msg @ TurbineMessage::ShredBatchHeader(_)) => {
                                    metrics::counter!("nusantara_turbine_batch_headers_received").increment(1);
                                    if message_sender.send((msg, src)).await.is_err() {
                                        debug!("message channel closed");
                                        break;
                                    }
                                }
                                Ok(msg @ TurbineMessage::Shred(_)) => {
                                    metrics::counter!("nusantara_turbine_shreds_received_total").increment(1);
                                    if message_sender.send((msg, src)).await.is_err() {
                                        debug!("message channel closed");
                                        break;
                                    }
                                }
                                Ok(msg @ TurbineMessage::RepairResponse(_)) => {
                                    metrics::counter!("nusantara_turbine_repair_shreds_received").increment(1);
                                    if message_sender.send((msg, src)).await.is_err() {
                                        debug!("message channel closed");
                                        break;
                                    }
                                }
                                Ok(TurbineMessage::BatchRepairResponse(batch)) => {
                                    // Cap fan-out: one packet cannot flood the channel
                                    // with more than MAX_BATCH_RESPONSE_SHREDS sends.
                                    if batch.shreds.len() > MAX_BATCH_RESPONSE_SHREDS {
                                        warn!(
                                            %src,
                                            count = batch.shreds.len(),
                                            max = MAX_BATCH_RESPONSE_SHREDS,
                                            "BatchRepairResponse exceeds shred cap, dropping"
                                        );
                                        metrics::counter!(
                                            "nusantara_turbine_batch_response_dropped_cap"
                                        )
                                        .increment(1);
                                        continue;
                                    }
                                    let count = batch.shreds.len() as u64;
                                    metrics::counter!("nusantara_turbine_repair_shreds_received")
                                        .increment(count);
                                    for shred in batch.shreds {
                                        let msg = TurbineMessage::Shred(shred);
                                        if message_sender.send((msg, src)).await.is_err() {
                                            debug!("message channel closed");
                                            return;
                                        }
                                    }
                                }
                                Ok(msg @ TurbineMessage::RepairRequest(_)) => {
                                    let _ = repair_sender.send((msg, src)).await;
                                }
                                Err(e) => {
                                    debug!(%src, error = %e, "failed to deserialize turbine message");
                                }
                            }
                        }
                        Err(e) => {
                            consecutive_errors += 1;
                            error!(
                                error = %e,
                                consecutive = consecutive_errors,
                                "turbine recv error"
                            );
                            if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                                error!(
                                    max = MAX_CONSECUTIVE_ERRORS,
                                    "too many consecutive UDP errors, stopping shred receiver"
                                );
                                break;
                            }
                            // Brief backoff to avoid spinning on a broken socket
                            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                        }
                    }
                }
                _ = shutdown.changed() => {
                    break;
                }
            }
        }
    }
}
