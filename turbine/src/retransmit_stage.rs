use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use nusantara_core::block::Block;
use nusantara_crypto::{Hash, PublicKey};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, instrument, warn};

use crate::merkle_shred::MerkleShred;
use crate::protocol::TurbineMessage;
use crate::shred_collector::ShredCollector;
use crate::turbine_tree::TurbineTree;

/// Shreds older than this many slots behind the tip are not retransmitted.
const RETRANSMIT_SLOT_HORIZON: u64 = 64;

pub struct RetransmitStage {
    my_identity: Hash,
    socket: Arc<UdpSocket>,
    collector: Arc<ShredCollector>,
    current_slot: Arc<AtomicU64>,
}

impl RetransmitStage {
    pub fn new(
        my_identity: Hash,
        socket: Arc<UdpSocket>,
        collector: Arc<ShredCollector>,
        current_slot: Arc<AtomicU64>,
    ) -> Self {
        Self {
            my_identity,
            socket,
            collector,
            current_slot,
        }
    }

    /// Run the retransmit loop.
    /// Now receives full `TurbineMessage` (including headers) instead of just shreds.
    #[instrument(skip_all, name = "retransmit")]
    pub async fn run<T, A, P>(
        self,
        mut message_receiver: mpsc::Receiver<(TurbineMessage, SocketAddr)>,
        block_sender: mpsc::Sender<Block>,
        tree_provider: T,
        addr_lookup: A,
        pubkey_lookup: P,
        mut shutdown: watch::Receiver<bool>,
    ) where
        T: Fn(u64) -> Option<TurbineTree>,
        A: Fn(&Hash) -> Option<SocketAddr>,
        P: Fn(&Hash) -> Option<PublicKey>,
    {
        loop {
            tokio::select! {
                biased;
                Some((msg, _src)) = message_receiver.recv() => {
                    match msg {
                        TurbineMessage::ShredBatchHeader(header) => {
                            let slot = header.slot;
                            let leader = header.leader;

                            // Skip already-stored slots (no verification or retransmit needed)
                            if self.collector.is_slot_stored(slot) {
                                metrics::counter!("nusantara_turbine_retransmit_skipped_stored").increment(1);
                                continue;
                            }

                            // Skip stale slots far behind the chain tip
                            let current = self.current_slot.load(Ordering::Relaxed);
                            if current > RETRANSMIT_SLOT_HORIZON && slot < current - RETRANSMIT_SLOT_HORIZON {
                                metrics::counter!("nusantara_turbine_retransmit_skipped_stale").increment(1);
                                continue;
                            }

                            // Verify header signature (1 Dilithium3 verify per slot)
                            let Some(pubkey) = pubkey_lookup(&leader) else {
                                warn!(slot, leader = ?leader, "dropping header from unknown leader");
                                metrics::counter!("nusantara_turbine_shreds_unknown_leader").increment(1);
                                continue;
                            };

                            if !header.verify(&pubkey) {
                                warn!(slot, leader = ?leader, "dropping header with invalid signature");
                                metrics::counter!("nusantara_turbine_invalid_shred_signatures").increment(1);
                                continue;
                            }

                            metrics::counter!("nusantara_turbine_batch_headers_verified").increment(1);

                            // Store header in collector. If shreds arrived before
                            // the header, retroactive Merkle verification runs and
                            // block assembly may complete here.
                            if let Some(block) = self.collector.insert_header(header.clone()) {
                                info!(
                                    slot = block.header.slot,
                                    txs = block.header.transaction_count,
                                    "block assembled from buffered shreds after header arrival"
                                );
                                if block_sender.send(block).await.is_err() {
                                    debug!("block channel closed");
                                    break;
                                }
                            }

                            // Retransmit header to downstream peers
                            if let Some(tree) = tree_provider(slot) {
                                let peer_ids = tree.retransmit_peers(&self.my_identity);
                                let peer_addrs: Vec<SocketAddr> = peer_ids
                                    .iter()
                                    .filter_map(&addr_lookup)
                                    .collect();
                                if !peer_addrs.is_empty() {
                                    let retransmit_msg = TurbineMessage::ShredBatchHeader(header);
                                    self.retransmit_message(&retransmit_msg, &peer_addrs).await;
                                }
                            }
                        }

                        TurbineMessage::Shred(shred) | TurbineMessage::RepairResponse(shred) => {
                            let slot = shred.slot();

                            // Skip already-stored slots
                            if self.collector.is_slot_stored(slot) {
                                metrics::counter!("nusantara_turbine_retransmit_skipped_stored").increment(1);
                                continue;
                            }

                            // Skip stale slots far behind the chain tip
                            let current = self.current_slot.load(Ordering::Relaxed);
                            if current > RETRANSMIT_SLOT_HORIZON && slot < current - RETRANSMIT_SLOT_HORIZON {
                                metrics::counter!("nusantara_turbine_retransmit_skipped_stale").increment(1);
                                continue;
                            }

                            // Verify via Merkle proof (fast hash ops, no Dilithium3)
                            if let Some(merkle_root) = self.collector.get_merkle_root(slot)
                                && !shred.verify(&merkle_root)
                            {
                                warn!(slot, index = shred.index(), "dropping shred with invalid merkle proof");
                                metrics::counter!("nusantara_turbine_invalid_shred_signatures").increment(1);
                                continue;
                            }
                            // If no header yet, shred is buffered — verified retroactively when header arrives

                            // Retransmit to downstream peers
                            if let Some(tree) = tree_provider(slot) {
                                let peer_ids = tree.retransmit_peers(&self.my_identity);
                                let peer_addrs: Vec<SocketAddr> = peer_ids
                                    .iter()
                                    .filter_map(&addr_lookup)
                                    .collect();
                                if !peer_addrs.is_empty() {
                                    let retransmit_msg = TurbineMessage::Shred(shred.clone());
                                    self.retransmit_message(&retransmit_msg, &peer_addrs).await;
                                }
                            }

                            // Feed data shreds to collector
                            if let MerkleShred::Data(ref data_shred) = shred
                                && let Some(block) = self.collector.insert_data_shred(data_shred)
                            {
                                info!(
                                    slot = block.header.slot,
                                    txs = block.header.transaction_count,
                                    "block assembled from shreds"
                                );
                                if block_sender.send(block).await.is_err() {
                                    debug!("block channel closed");
                                    break;
                                }
                            }

                            metrics::counter!("nusantara_turbine_retransmit_total").increment(1);
                        }

                        // Repair requests, batch responses — handled by other tasks
                        _ => {}
                    }
                }
                _ = shutdown.changed() => {
                    break;
                }
            }
        }
    }

    async fn retransmit_message(&self, msg: &TurbineMessage, peer_addrs: &[SocketAddr]) {
        let bytes = match msg.serialize_to_bytes() {
            Ok(b) => b,
            Err(e) => {
                debug!(error = %e, "failed to serialize retransmit message");
                return;
            }
        };

        for addr in peer_addrs {
            if let Err(e) = self.socket.send_to(&bytes, addr).await {
                debug!(%addr, error = %e, "retransmit send failed");
            }
        }
    }
}
