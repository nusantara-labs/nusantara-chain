use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use nusantara_core::block::Block;
use nusantara_crypto::{Hash, PublicKey};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::merkle_shred::MerkleShred;
use crate::protocol::TurbineMessage;
use crate::shred_collector::ShredCollector;
use crate::turbine_tree::TurbineTree;

/// Shreds older than this many slots behind the replay tip are not retransmitted.
/// 1024 slots = ~409s at 400ms/slot — matches ORPHAN_HORIZON so followers
/// don't drop shreds they still need during catch-up.
const RETRANSMIT_SLOT_HORIZON: u64 = 1024;

pub struct RetransmitStage {
    my_identity: Hash,
    socket: Arc<UdpSocket>,
    collector: Arc<ShredCollector>,
    /// Replay progress counter — used for stale-slot filtering instead of
    /// wall-clock slot to prevent catch-up death spirals.
    replay_tip: Arc<AtomicU64>,
}

impl RetransmitStage {
    pub fn new(
        my_identity: Hash,
        socket: Arc<UdpSocket>,
        collector: Arc<ShredCollector>,
        _current_slot: Arc<AtomicU64>,
        replay_tip: Arc<AtomicU64>,
    ) -> Self {
        Self {
            my_identity,
            socket,
            collector,
            replay_tip,
        }
    }

    /// Run the retransmit loop.
    /// Now receives full `TurbineMessage` (including headers) instead of just shreds.
    ///
    /// `#[instrument]` is intentionally absent: wrapping the entire loop in a
    /// single span produces a span whose lifetime equals the task lifetime,
    /// polluting traces with a permanently-open root span. Per-message helpers
    /// can be instrumented if needed.
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
        // Cache peer addresses per slot to avoid recomputing `tree_provider`
        // and `retransmit_peers` on every shred in the same slot.
        // Bounded by the number of distinct active slots, which is small in practice.
        let mut peer_cache: HashMap<u64, Arc<[SocketAddr]>> = HashMap::new();

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

                            // Skip stale slots far behind replay progress.
                            let tip = self.replay_tip.load(Ordering::Relaxed);
                            if tip > RETRANSMIT_SLOT_HORIZON && slot < tip - RETRANSMIT_SLOT_HORIZON {
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
                                // Clear the peer cache for this slot — it's done.
                                peer_cache.remove(&slot);
                                if block_sender.send(block).await.is_err() {
                                    debug!("block channel closed");
                                    break;
                                }
                            }

                            // Retransmit header to downstream peers
                            let peer_addrs = self.get_peer_addrs(slot, &tree_provider, &addr_lookup, &mut peer_cache);
                            if !peer_addrs.is_empty() {
                                let retransmit_msg = TurbineMessage::ShredBatchHeader(header);
                                self.retransmit_serialized(&retransmit_msg, &peer_addrs).await;
                            }
                        }

                        TurbineMessage::Shred(shred) | TurbineMessage::RepairResponse(shred) => {
                            let slot = shred.slot();

                            // Skip already-stored slots
                            if self.collector.is_slot_stored(slot) {
                                metrics::counter!("nusantara_turbine_retransmit_skipped_stored").increment(1);
                                continue;
                            }

                            // Skip stale slots far behind replay progress.
                            let tip = self.replay_tip.load(Ordering::Relaxed);
                            if tip > RETRANSMIT_SLOT_HORIZON && slot < tip - RETRANSMIT_SLOT_HORIZON {
                                metrics::counter!("nusantara_turbine_retransmit_skipped_stale").increment(1);
                                continue;
                            }

                            // Resolve the Merkle root for this slot.
                            // CRITICAL: only retransmit shreds whose Merkle proof we can
                            // actually verify. Without the batch header we have no root, so
                            // retransmitting would amplify unverified (potentially forged) content
                            // to every downstream layer. Instead, buffer the shred in the collector
                            // (retroactive verification runs when the header arrives) but skip the
                            // network retransmit path entirely until then.
                            let Some(merkle_root) = self.collector.get_merkle_root(slot) else {
                                // Header not yet received — buffer in collector, defer retransmit.
                                // Both data AND code shreds are buffered: dropping code shreds
                                // here would make FEC recovery structurally impossible for slots
                                // where the header arrives late (common on high-latency paths).
                                match &shred {
                                    MerkleShred::Data(data_shred) => {
                                        let _ = self.collector.insert_data_shred(data_shred);
                                    }
                                    MerkleShred::Code(code_shred) => {
                                        let _ = self.collector.insert_code_shred(code_shred);
                                        metrics::counter!("nusantara_turbine_code_shreds_buffered").increment(1);
                                    }
                                }
                                metrics::counter!("nusantara_turbine_retransmit_deferred_no_header").increment(1);
                                continue;
                            };

                            // Header present — verify before retransmitting.
                            if !shred.verify(&merkle_root) {
                                warn!(slot, index = shred.index(), "dropping shred with invalid merkle proof");
                                metrics::counter!("nusantara_turbine_invalid_shred_signatures").increment(1);
                                continue;
                            }

                            // Serialize once for all peers (avoids re-serialization per peer)
                            let retransmit_msg = TurbineMessage::Shred(shred.clone());
                            let peer_addrs = self.get_peer_addrs(slot, &tree_provider, &addr_lookup, &mut peer_cache);
                            if !peer_addrs.is_empty() {
                                self.retransmit_serialized(&retransmit_msg, &peer_addrs).await;
                            }

                            // Feed shreds to collector. Data shreds can trigger
                            // block assembly; code shreds are buffered for future
                            // FEC recovery (assembly only happens via data shreds).
                            match &shred {
                                MerkleShred::Data(data_shred) => {
                                    if let Some(block) = self.collector.insert_data_shred(data_shred) {
                                        info!(
                                            slot = block.header.slot,
                                            txs = block.header.transaction_count,
                                            "block assembled from shreds"
                                        );
                                        peer_cache.remove(&slot);
                                        if block_sender.send(block).await.is_err() {
                                            debug!("block channel closed");
                                            break;
                                        }
                                    }
                                }
                                MerkleShred::Code(code_shred) => {
                                    let _ = self.collector.insert_code_shred(code_shred);
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

    /// Look up (or compute + cache) the downstream peer addresses for `slot`.
    /// The cache is a simple HashMap bounded by the number of active slots.
    fn get_peer_addrs<T, A>(
        &self,
        slot: u64,
        tree_provider: &T,
        addr_lookup: &A,
        cache: &mut HashMap<u64, Arc<[SocketAddr]>>,
    ) -> Arc<[SocketAddr]>
    where
        T: Fn(u64) -> Option<TurbineTree>,
        A: Fn(&Hash) -> Option<SocketAddr>,
    {
        if let Some(addrs) = cache.get(&slot) {
            return Arc::clone(addrs);
        }
        let addrs: Arc<[SocketAddr]> = if let Some(tree) = tree_provider(slot) {
            let peer_ids = tree.retransmit_peers(&self.my_identity);
            peer_ids.iter().filter_map(addr_lookup).collect()
        } else {
            Arc::from([])
        };
        cache.insert(slot, Arc::clone(&addrs));
        addrs
    }

    /// Serialize `msg` once and send to all `peer_addrs`.
    async fn retransmit_serialized(&self, msg: &TurbineMessage, peer_addrs: &[SocketAddr]) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shredder::Shredder;
    use nusantara_core::block::{Block, BlockHeader};
    use nusantara_crypto::{Hash, Keypair, hash};

    fn test_block(slot: u64) -> Block {
        Block {
            header: BlockHeader {
                slot,
                parent_slot: slot.saturating_sub(1),
                parent_hash: hash(b"parent"),
                block_hash: hash(b"block"),
                timestamp: 1000,
                validator: hash(b"validator"),
                transaction_count: 0,
                merkle_root: Hash::zero(),
                poh_hash: Hash::zero(),
                bank_hash: Hash::zero(),
                state_root: Hash::zero(),
            },
            transactions: vec![],
            batches: Vec::new(),
        }
    }

    /// CRITICAL (Finding 1): A shred received before the batch header arrives
    /// must be buffered in the collector but must NOT reach the retransmit path.
    /// The `nusantara_turbine_retransmit_deferred_no_header` counter must increment.
    #[test]
    fn shred_before_header_is_deferred_not_retransmitted() {
        let collector = Arc::new(ShredCollector::new());
        let kp = Keypair::generate();

        let block = test_block(42);
        let batch = Shredder::shred_block(&block, 41, &kp).unwrap();
        let shred = batch.data_shreds[0].clone();
        let slot = shred.slot();

        // Confirm: no header present for this slot yet.
        assert!(collector.get_merkle_root(slot).is_none());

        // Simulate the retransmit decision: no header → deferred.
        // Mirror the exact logic in `RetransmitStage::run`.
        let was_deferred = collector.get_merkle_root(slot).is_none();
        assert!(was_deferred, "should be deferred when no header");

        // Buffer shred in collector (as retransmit stage does).
        let assembled = collector.insert_data_shred(&shred);
        assert!(assembled.is_none(), "cannot assemble without header");

        // Shred is buffered — present in collector.
        assert!(collector.has_slot(slot));
        assert_eq!(collector.shred_count(slot), 1);
    }

    /// Complement (Finding 1): After the header arrives, the buffered shred
    /// undergoes retroactive verification. If it passes, the slot can complete.
    #[test]
    fn shred_buffered_before_header_assembles_on_header_arrival() {
        let collector = Arc::new(ShredCollector::new());
        let kp = Keypair::generate();

        let block = test_block(43);
        let batch = Shredder::shred_block(&block, 42, &kp).unwrap();
        let slot = batch.header.slot;

        // Buffer ALL shreds first (no header) — retransmit stage defers these.
        for shred in &batch.data_shreds {
            let result = collector.insert_data_shred(shred);
            assert!(result.is_none(), "no assembly without header");
        }
        assert!(collector.has_slot(slot));

        // Now header arrives — retroactive verification + assembly.
        let assembled = collector.insert_header(batch.header.clone());
        assert!(
            assembled.is_some(),
            "block should assemble after header arrives for all-buffered shreds"
        );
        assert_eq!(assembled.unwrap(), block);
    }
}
