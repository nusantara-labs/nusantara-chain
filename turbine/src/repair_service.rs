use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use nusantara_core::native_token::const_parse_u64;
use rand::prelude::IndexedRandom;
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tracing::{debug, info, instrument};

use crate::protocol::{RepairRequest, TurbineMessage};
use crate::shred_collector::ShredCollector;

pub const REPAIR_INTERVAL_MS: u64 = const_parse_u64(env!("NUSA_TURBINE_REPAIR_INTERVAL_MS"));
pub const MAX_REPAIR_BATCH_REQUEST: u64 =
    const_parse_u64(env!("NUSA_TURBINE_MAX_REPAIR_BATCH_REQUEST"));

/// Slots older than this relative to current_slot are evicted from the
/// ShredCollector on each repair tick.
const MAX_REPAIR_SLOT_AGE: u64 = 64;

/// Maximum number of slots to request repairs for per tick.
/// With 200ms tick interval, this paces repair to 20 slots/second.
const MAX_REPAIR_SLOTS_PER_TICK: usize = 16;

/// Number of random peers to send each repair request to.
const REPAIR_PEER_SAMPLE_SIZE: usize = 2;

/// Minimum time (ms) between repair requests for the same slot.
const REPAIR_COOLDOWN_MS: u64 = 1000;

/// Select up to `count` random peers from the list.
fn sample_peers(peers: &[SocketAddr], count: usize) -> Vec<SocketAddr> {
    if peers.len() <= count {
        return peers.to_vec();
    }
    let mut rng = rand::rng();
    peers.choose_multiple(&mut rng, count).copied().collect()
}

pub struct RepairService {
    socket: Arc<UdpSocket>,
    collector: Arc<ShredCollector>,
    current_slot: Arc<AtomicU64>,
}

impl RepairService {
    pub fn new(
        socket: Arc<UdpSocket>,
        collector: Arc<ShredCollector>,
        current_slot: Arc<AtomicU64>,
    ) -> Self {
        Self {
            socket,
            collector,
            current_slot,
        }
    }

    #[instrument(skip(self, repair_peers_fn, shutdown), name = "repair_service")]
    pub async fn run<F>(
        self,
        repair_peers_fn: F,
        mut shutdown: watch::Receiver<bool>,
    ) where
        F: Fn() -> Vec<SocketAddr>,
    {
        let interval = tokio::time::Duration::from_millis(REPAIR_INTERVAL_MS);
        let mut tick = tokio::time::interval(interval);
        let mut last_repair: HashMap<u64, Instant> = HashMap::new();

        loop {
            tokio::select! {
                biased;
                _ = tick.tick() => {
                    let current = self.current_slot.load(Ordering::Relaxed);
                    let evicted = self.collector.cleanup_old_slots(current, MAX_REPAIR_SLOT_AGE);

                    // Purge cooldown entries for slots no longer tracked
                    let tracked: std::collections::HashSet<u64> = self.collector.tracked_slots().into_iter().collect();
                    last_repair.retain(|slot, _| tracked.contains(slot));
                    if evicted > 0 {
                        debug!(evicted, current, "evicted stale slots from shred collector");
                    }

                    let mut slots = self.collector.tracked_slots();
                    // Prioritize most recent slots, cap per tick to prevent burst
                    slots.sort_unstable_by(|a, b| b.cmp(a));
                    let deferred = slots.len().saturating_sub(MAX_REPAIR_SLOTS_PER_TICK);
                    if deferred > 0 {
                        metrics::counter!("nusantara_turbine_repair_slots_deferred").increment(deferred as u64);
                    }
                    slots.truncate(MAX_REPAIR_SLOTS_PER_TICK);

                    let peers = repair_peers_fn();

                    if peers.is_empty() {
                        continue;
                    }

                    if !slots.is_empty() {
                        info!(tracked_slots = slots.len(), peers = peers.len(), "repair tick");
                    }

                    for slot in &slots {
                        // Skip slots that were recently repaired (cooldown)
                        let now = Instant::now();
                        if let Some(last) = last_repair.get(slot)
                            && now.duration_since(*last).as_millis() < REPAIR_COOLDOWN_MS as u128
                        {
                            metrics::counter!("nusantara_turbine_repair_cooldown_skipped").increment(1);
                            continue;
                        }

                        let selected = sample_peers(&peers, REPAIR_PEER_SAMPLE_SIZE);

                        // Request batch header if missing
                        if !self.collector.has_header(*slot) && self.collector.shred_count(*slot) > 0 {
                            let req = TurbineMessage::RepairRequest(
                                RepairRequest::BatchHeader { slot: *slot },
                            );
                            if let Ok(bytes) = req.serialize_to_bytes() {
                                for peer in &selected {
                                    let _ = self.socket.send_to(&bytes, peer).await;
                                }
                            }
                            debug!(slot = *slot, "requesting missing batch header");
                            metrics::counter!("nusantara_turbine_repair_requests_total").increment(1);
                        }

                        let missing = self.collector.missing_shreds(*slot);

                        if missing.is_empty() && self.collector.shred_count(*slot) == 0 {
                            let req = TurbineMessage::RepairRequest(
                                RepairRequest::HighestShred { slot: *slot },
                            );
                            if let Ok(bytes) = req.serialize_to_bytes() {
                                for peer in &selected {
                                    let _ = self.socket.send_to(&bytes, peer).await;
                                }
                                debug!(slot = *slot, peers = selected.len(), "broadcast HighestShred repair request");
                            }
                            metrics::counter!("nusantara_turbine_repair_requests_total").increment(1);
                            last_repair.insert(*slot, now);
                            continue;
                        }

                        if missing.is_empty() {
                            if self.collector.is_slot_complete(*slot) {
                                continue;
                            }
                            let req = TurbineMessage::RepairRequest(
                                RepairRequest::HighestShred { slot: *slot },
                            );
                            if let Ok(bytes) = req.serialize_to_bytes() {
                                for peer in &selected {
                                    let _ = self.socket.send_to(&bytes, peer).await;
                                }
                            }
                            debug!(
                                slot = *slot,
                                shred_count = self.collector.shred_count(*slot),
                                "requesting HighestShred — have shreds but missing last index"
                            );
                            metrics::counter!("nusantara_turbine_repair_requests_total").increment(1);
                            last_repair.insert(*slot, now);
                            continue;
                        }

                        debug!(slot, missing_count = missing.len(), "requesting batch repair shreds");

                        for chunk in missing.chunks(MAX_REPAIR_BATCH_REQUEST as usize) {
                            let req = TurbineMessage::RepairRequest(
                                RepairRequest::ShredBatch {
                                    slot: *slot,
                                    indices: chunk.to_vec(),
                                },
                            );
                            if let Ok(bytes) = req.serialize_to_bytes() {
                                for peer in &selected {
                                    let _ = self.socket.send_to(&bytes, peer).await;
                                }
                            }
                        }

                        let chunk_count =
                            missing.len().div_ceil(MAX_REPAIR_BATCH_REQUEST as usize);
                        metrics::counter!("nusantara_turbine_repair_requests_total")
                            .increment(chunk_count as u64);
                        last_repair.insert(*slot, now);
                    }
                }
                _ = shutdown.changed() => {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr};

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port)
    }

    #[test]
    fn sample_peers_returns_all_when_fewer_than_count() {
        let peers = vec![addr(1000), addr(1001)];
        let result = sample_peers(&peers, 5);
        assert_eq!(result.len(), 2);
        assert!(result.contains(&addr(1000)));
        assert!(result.contains(&addr(1001)));
    }

    #[test]
    fn sample_peers_returns_exact_count() {
        let peers: Vec<SocketAddr> = (1000..1010).map(addr).collect();
        let result = sample_peers(&peers, 2);
        assert_eq!(result.len(), 2);
        // All returned peers must be from the original list
        for p in &result {
            assert!(peers.contains(p));
        }
    }

    #[test]
    fn sample_peers_empty_input() {
        let result = sample_peers(&[], 3);
        assert!(result.is_empty());
    }

    #[test]
    fn sample_peers_exact_match() {
        let peers = vec![addr(1000), addr(1001), addr(1002)];
        let result = sample_peers(&peers, 3);
        assert_eq!(result.len(), 3);
    }
}
