use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, instrument};

use crate::protocol::{TurbineMessage, MAX_UDP_PACKET};

pub struct ShredReceiver {
    socket: Arc<UdpSocket>,
}

impl ShredReceiver {
    pub fn new(socket: Arc<UdpSocket>) -> Self {
        Self { socket }
    }

    /// Receive turbine messages from the UDP socket and forward to the retransmit stage.
    /// Passes full `TurbineMessage` (not just shreds) so that headers can flow through.
    #[instrument(skip(self, message_sender, repair_sender, shutdown), name = "shred_receiver")]
    pub async fn run(
        self,
        message_sender: mpsc::Sender<(TurbineMessage, SocketAddr)>,
        repair_sender: mpsc::Sender<(TurbineMessage, SocketAddr)>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let mut buf = vec![0u8; MAX_UDP_PACKET];

        loop {
            tokio::select! {
                biased;
                result = self.socket.recv_from(&mut buf) => {
                    match result {
                        Ok((len, src)) => {
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
                                    let count = batch.shreds.len() as u64;
                                    metrics::counter!("nusantara_turbine_repair_shreds_received").increment(count);
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
                            error!(error = %e, "turbine recv error");
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
