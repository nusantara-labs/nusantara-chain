use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use nusantara_core::native_token::const_parse_u64;
use rand::prelude::IndexedRandom;
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tracing::{debug, info};

use crate::protocol::{RepairRequest, TurbineMessage};
use crate::shred_collector::ShredCollector;

pub const REPAIR_INTERVAL_MS: u64 = const_parse_u64(env!("NUSA_TURBINE_REPAIR_INTERVAL_MS"));
pub const MAX_REPAIR_BATCH_REQUEST: u64 =
    const_parse_u64(env!("NUSA_TURBINE_MAX_REPAIR_BATCH_REQUEST"));

/// Slots older than this relative to replay_tip are evicted from the ShredCollector on each tick.
pub const MAX_REPAIR_SLOT_AGE: u64 =
    const_parse_u64(env!("NUSA_TURBINE_MAX_REPAIR_SLOT_AGE"));

/// Maximum number of slots to request repairs for per tick.
/// With 200ms tick interval and this value, repair throughput is bounded.
pub const MAX_REPAIR_SLOTS_PER_TICK: usize =
    const_parse_u64(env!("NUSA_TURBINE_MAX_REPAIR_SLOTS_PER_TICK")) as usize;

/// Number of random peers to send each repair request to.
pub const REPAIR_PEER_SAMPLE_SIZE: usize =
    const_parse_u64(env!("NUSA_TURBINE_REPAIR_PEER_SAMPLE_SIZE")) as usize;

/// Minimum time (ms) between repair requests for the same slot.
pub const REPAIR_COOLDOWN_MS: u64 = const_parse_u64(env!("NUSA_TURBINE_REPAIR_COOLDOWN_MS"));

/// Select up to `count` random peers from the list.
fn sample_peers(peers: &[SocketAddr], count: usize) -> Vec<SocketAddr> {
    if peers.len() <= count {
        return peers.to_vec();
    }
    let mut rng = rand::rng();
    peers.sample(&mut rng, count).copied().collect()
}

pub struct RepairService {
    socket: Arc<UdpSocket>,
    collector: Arc<ShredCollector>,
    current_slot: Arc<AtomicU64>,
    /// Replay progress counter — used for eviction instead of wall-clock slot
    /// to prevent catch-up death spirals when the validator falls behind.
    replay_tip: Arc<AtomicU64>,
}

impl RepairService {
    pub fn new(
        socket: Arc<UdpSocket>,
        collector: Arc<ShredCollector>,
        current_slot: Arc<AtomicU64>,
        replay_tip: Arc<AtomicU64>,
    ) -> Self {
        Self {
            socket,
            collector,
            current_slot,
            replay_tip,
        }
    }

    /// Run the repair loop.
    ///
    /// `#[instrument]` is intentionally absent: wrapping the entire loop in a
    /// single span produces a span whose lifetime equals the task lifetime,
    /// polluting traces. Instrument inner helpers if per-tick tracing is needed.
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
        let mut first_tracked: HashMap<u64, Instant> = HashMap::new();

        loop {
            tokio::select! {
                biased;
                _ = tick.tick() => {
                    let current = self.current_slot.load(Ordering::Relaxed);
                    let replay_tip = self.replay_tip.load(Ordering::Relaxed);

                    let evicted =
                        self.collector.cleanup_old_slots(replay_tip, MAX_REPAIR_SLOT_AGE);

                    // Cache tracked_slots once — each call is an O(N) DashMap scan.
                    // Reuse the result for all operations in this tick.
                    let tracked = self.collector.tracked_slots();

                    // Evict tracked slots with 0 shreds that have been
                    // tracked for >=1s (empty/skipped slots).
                    {
                        let now = Instant::now();
                        for &s in &tracked {
                            first_tracked.entry(s).or_insert(now);
                        }
                        let stale: Vec<u64> = tracked
                            .iter()
                            .copied()
                            .filter(|&s| {
                                if self.collector.shred_count(s) > 0 {
                                    return false;
                                }
                                if s < replay_tip {
                                    return true;
                                }
                                first_tracked
                                    .get(&s)
                                    .is_some_and(|t| now.duration_since(*t).as_millis() >= 1000)
                            })
                            .collect();
                        for s in &stale {
                            self.collector.mark_slot_empty(*s);
                            self.collector.remove_slot(*s);
                            last_repair.remove(s);
                            first_tracked.remove(s);
                        }
                        if !stale.is_empty() {
                            debug!(
                                count = stale.len(),
                                replay_tip,
                                "evicted empty/stale tracked slots"
                            );
                            metrics::counter!("nusantara_turbine_empty_slots_evicted")
                                .increment(stale.len() as u64);
                        }
                        // Clean first_tracked for no-longer-tracked slots
                        let tracked_set: std::collections::HashSet<u64> =
                            tracked.iter().copied().collect();
                        first_tracked.retain(|s, _| tracked_set.contains(s));
                    }

                    // Re-fetch tracked after stale eviction — slots were removed above.
                    // This is the only second tracked_slots() call per tick.
                    let mut slots = self.collector.tracked_slots();

                    // Purge cooldown entries for slots no longer tracked
                    {
                        let slots_set: std::collections::HashSet<u64> =
                            slots.iter().copied().collect();
                        last_repair.retain(|slot, _| slots_set.contains(slot));
                    }

                    if evicted > 0 {
                        debug!(evicted, current, "evicted stale slots from shred collector");
                    }

                    // Prioritise: catch-up → lowest slots first; steady state → newest first.
                    let catching_up = current > replay_tip + 128;
                    if catching_up {
                        slots.sort_unstable();
                    } else {
                        slots.sort_unstable_by(|a, b| b.cmp(a));
                    }
                    let deferred = slots.len().saturating_sub(MAX_REPAIR_SLOTS_PER_TICK);
                    if deferred > 0 {
                        metrics::counter!("nusantara_turbine_repair_slots_deferred")
                            .increment(deferred as u64);
                    }
                    slots.truncate(MAX_REPAIR_SLOTS_PER_TICK);

                    let peers = repair_peers_fn();

                    if peers.is_empty() {
                        continue;
                    }

                    if !slots.is_empty() {
                        info!(tracked_slots = slots.len(), peers = peers.len(), "repair tick");
                    }

                    let effective_cooldown = if catching_up { 50u128 } else { REPAIR_COOLDOWN_MS as u128 };
                    let effective_peer_sample =
                        if catching_up { 3 } else { REPAIR_PEER_SAMPLE_SIZE };

                    for slot in &slots {
                        let now = Instant::now();
                        if let Some(last) = last_repair.get(slot)
                            && now.duration_since(*last).as_millis() < effective_cooldown
                        {
                            metrics::counter!("nusantara_turbine_repair_cooldown_skipped")
                                .increment(1);
                            continue;
                        }

                        let selected = sample_peers(&peers, effective_peer_sample);

                        // Request batch header if missing
                        if !self.collector.has_header(*slot)
                            && self.collector.shred_count(*slot) > 0
                        {
                            let req = TurbineMessage::RepairRequest(RepairRequest::BatchHeader {
                                slot: *slot,
                            });
                            if let Ok(bytes) = req.serialize_to_bytes() {
                                for peer in &selected {
                                    let _ = self.socket.send_to(&bytes, peer).await;
                                }
                            }
                            debug!(slot = *slot, "requesting missing batch header");
                            metrics::counter!("nusantara_turbine_repair_requests_total")
                                .increment(1);
                        }

                        let missing = self.collector.missing_shreds(*slot);

                        if missing.is_empty() && self.collector.shred_count(*slot) == 0 {
                            let req = TurbineMessage::RepairRequest(RepairRequest::HighestShred {
                                slot: *slot,
                            });
                            if let Ok(bytes) = req.serialize_to_bytes() {
                                for peer in &selected {
                                    let _ = self.socket.send_to(&bytes, peer).await;
                                }
                                debug!(
                                    slot = *slot,
                                    peers = selected.len(),
                                    "broadcast HighestShred repair request"
                                );
                            }
                            metrics::counter!("nusantara_turbine_repair_requests_total")
                                .increment(1);
                            last_repair.insert(*slot, now);
                            continue;
                        }

                        if missing.is_empty() {
                            if self.collector.is_slot_complete(*slot) {
                                continue;
                            }
                            let req = TurbineMessage::RepairRequest(RepairRequest::HighestShred {
                                slot: *slot,
                            });
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
                            metrics::counter!("nusantara_turbine_repair_requests_total")
                                .increment(1);
                            last_repair.insert(*slot, now);
                            continue;
                        }

                        debug!(slot, missing_count = missing.len(), "requesting batch repair shreds");

                        for chunk in missing.chunks(MAX_REPAIR_BATCH_REQUEST as usize) {
                            let req = TurbineMessage::RepairRequest(RepairRequest::ShredBatch {
                                slot: *slot,
                                indices: chunk.to_vec(),
                            });
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
        for p in &result {
            assert!(peers.contains(p));
        }
    }

    #[test]
    fn config_constants_from_toml() {
        assert_eq!(MAX_REPAIR_SLOT_AGE, 1024);
        assert_eq!(MAX_REPAIR_SLOTS_PER_TICK, 512);
        assert_eq!(REPAIR_PEER_SAMPLE_SIZE, 2);
        assert_eq!(REPAIR_COOLDOWN_MS, 1000);
    }
}
