use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use nusantara_core::native_token::const_parse_u64;
use nusantara_crypto::Hash;
use parking_lot::RwLock;

use crate::contact_info::ContactInfo;
use crate::crds::CrdsTable;
use crate::crds_value::CrdsValue;
use crate::weighted_shuffle::weighted_shuffle;

pub const PUSH_FANOUT: u64 = const_parse_u64(env!("NUSA_GOSSIP_PUSH_FANOUT"));
pub const MAX_CRDS_VALUES_PER_PUSH: u64 =
    const_parse_u64(env!("NUSA_GOSSIP_MAX_CRDS_VALUES_PER_PUSH"));

/// Maximum number of (pruner, origin) entries in the prune map.
pub const MAX_PRUNE_SET_SIZE: usize = 100_000;

pub struct CrdsGossipPush {
    my_identity: Hash,
    /// Monotonic cursor into the CRDS ordered index. AtomicU64 avoids the
    /// RwLock read-then-write race that could lose values (H4).
    push_cursor: AtomicU64,
    /// Timed prune map: (pruner, origin) → expiry Instant.
    /// Replaces the HashSet + periodic clear_prunes with per-entry TTL (H2).
    prune_map: RwLock<HashMap<(Hash, Hash), Instant>>,
}

impl CrdsGossipPush {
    pub fn new(my_identity: Hash) -> Self {
        Self {
            my_identity,
            push_cursor: AtomicU64::new(0),
            prune_map: RwLock::new(HashMap::new()),
        }
    }

    /// Collect new CRDS values since last push and build per-peer value lists.
    ///
    /// Per-value, per-peer filtering (H1): each peer receives only the subset
    /// of values for which it has not issued a prune. Peers with an empty
    /// subset after filtering are skipped rather than skipping the entire peer.
    pub fn new_push_messages(
        &self,
        crds: &CrdsTable,
        peers: &[(ContactInfo, u64)],
        seed: &Hash,
    ) -> Vec<(SocketAddr, Vec<CrdsValue>)> {
        if peers.is_empty() {
            return Vec::new();
        }

        // Snapshot cursor atomically: load, collect values, CAS to new_cursor.
        // If CAS fails another caller raced; we still use what we collected —
        // duplicate delivery is safe in gossip, data loss is not.
        let cursor = self.push_cursor.load(Ordering::Acquire);
        let new_cursor = crds.current_cursor();
        let new_values = crds.values_since(cursor);
        let _ = self.push_cursor.compare_exchange(
            cursor,
            new_cursor,
            Ordering::Release,
            Ordering::Relaxed,
        );

        if new_values.is_empty() {
            return Vec::new();
        }

        let values: Vec<CrdsValue> = new_values
            .into_iter()
            .take(MAX_CRDS_VALUES_PER_PUSH as usize)
            .collect();

        let stake_peers: Vec<(Hash, u64)> = peers
            .iter()
            .map(|(ci, stake)| (ci.identity(), *stake))
            .collect();
        let indices = weighted_shuffle(&stake_peers, seed);

        let prune_map = self.prune_map.read();
        let now = Instant::now();

        let mut result = Vec::new();
        let mut fanout_remaining = PUSH_FANOUT as usize;

        for i in indices {
            if fanout_remaining == 0 {
                break;
            }
            let peer_id = peers[i].0.identity();
            if peer_id == self.my_identity {
                continue;
            }

            // Per-value filtering: send only values not pruned for this peer (H1).
            let peer_values: Vec<CrdsValue> = values
                .iter()
                .filter(|v| {
                    prune_map
                        .get(&(peer_id, v.origin()))
                        .map(|exp| *exp <= now)
                        .unwrap_or(true)
                })
                .cloned()
                .collect();

            if peer_values.is_empty() {
                continue;
            }

            result.push((peers[i].0.gossip_addr.0, peer_values));
            fanout_remaining -= 1;
        }

        result
    }

    /// Record a prune: peer `pruner` doesn't want values from `origin` for `ttl`.
    pub fn process_prune(&self, pruner: Hash, origins: &[Hash], ttl: Duration) {
        let expiry = Instant::now() + ttl;
        let mut prune_map = self.prune_map.write();
        if prune_map.len() >= MAX_PRUNE_SET_SIZE {
            metrics::counter!("nusantara_gossip_prune_set_overflow_total").increment(1);
            return;
        }
        for origin in origins {
            prune_map.insert((pruner, *origin), expiry);
        }
        metrics::counter!("nusantara_gossip_prune_messages_total").increment(1);
    }

    /// Evict prune entries whose TTL has expired (H2). Called from the purge loop.
    pub fn purge_expired_prunes(&self, now: Instant) {
        self.prune_map.write().retain(|_, expiry| *expiry > now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crds::CrdsTable;
    use crate::crds_value::{CrdsData, CrdsValue};
    use nusantara_crypto::{Keypair, hash};

    fn make_peer(i: i32) -> (ContactInfo, u64) {
        let kp = Keypair::generate();
        let ci = ContactInfo::new(
            kp.public_key().clone(),
            format!("127.0.0.1:{}", 8000 + i).parse().unwrap(),
            format!("127.0.0.1:{}", 9000 + i).parse().unwrap(),
            format!("127.0.0.1:{}", 9100 + i).parse().unwrap(),
            format!("127.0.0.1:{}", 9200 + i).parse().unwrap(),
            format!("127.0.0.1:{}", 9300 + i).parse().unwrap(),
            1,
            1000,
        );
        // Default stake 0: zero-stake peers are still shuffled (H5).
        (ci, 0)
    }

    fn make_peer_with_stake(i: i32, stake: u64) -> (ContactInfo, u64) {
        let (ci, _) = make_peer(i);
        (ci, stake)
    }

    #[test]
    fn config_values() {
        assert_eq!(PUSH_FANOUT, 6);
        assert_eq!(MAX_CRDS_VALUES_PER_PUSH, 10);
    }

    #[test]
    fn push_empty_peers() {
        let push = CrdsGossipPush::new(hash(b"me"));
        let crds = CrdsTable::new();
        let msgs = push.new_push_messages(&crds, &[], &hash(b"seed"));
        assert!(msgs.is_empty());
    }

    #[test]
    fn push_with_new_values() {
        let kp = Keypair::generate();
        let my_identity = kp.address();
        let push = CrdsGossipPush::new(my_identity);
        let crds = CrdsTable::new();

        let ci = ContactInfo::new(
            kp.public_key().clone(),
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.1:8003".parse().unwrap(),
            "127.0.0.1:8004".parse().unwrap(),
            "127.0.0.1:8001".parse().unwrap(),
            "127.0.0.1:8002".parse().unwrap(),
            1,
            1000,
        );
        crds.insert(CrdsValue::new_signed(CrdsData::ContactInfo(ci), &kp))
            .unwrap();

        let peers: Vec<(ContactInfo, u64)> =
            (0..10).map(|i| make_peer_with_stake(i, 1000)).collect();
        let msgs = push.new_push_messages(&crds, &peers, &hash(b"seed"));
        assert!(!msgs.is_empty());
        assert!(msgs.len() <= PUSH_FANOUT as usize);
    }

    #[test]
    fn zero_stake_peers_still_receive_values() {
        let kp = Keypair::generate();
        let my_identity = kp.address();
        let push = CrdsGossipPush::new(my_identity);
        let crds = CrdsTable::new();

        let ci = ContactInfo::new(
            kp.public_key().clone(),
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.1:8003".parse().unwrap(),
            "127.0.0.1:8004".parse().unwrap(),
            "127.0.0.1:8001".parse().unwrap(),
            "127.0.0.1:8002".parse().unwrap(),
            1,
            1000,
        );
        crds.insert(CrdsValue::new_signed(CrdsData::ContactInfo(ci), &kp))
            .unwrap();

        // All peers have zero stake (H5).
        let peers: Vec<(ContactInfo, u64)> = (0..10).map(make_peer).collect();
        let msgs = push.new_push_messages(&crds, &peers, &hash(b"seed"));
        assert!(
            !msgs.is_empty(),
            "zero-stake peers must still receive pushes"
        );
    }

    #[test]
    fn per_value_per_peer_prune_filtering() {
        let kp_me = Keypair::generate();
        let my_identity = kp_me.address();
        let push = CrdsGossipPush::new(my_identity);
        let crds = CrdsTable::new();

        // Insert two values with different origins.
        let kp_a = Keypair::generate();
        let kp_b = Keypair::generate();

        let make_ci = |kp: &Keypair, port: u16| {
            CrdsValue::new_signed(
                CrdsData::ContactInfo(ContactInfo::new(
                    kp.public_key().clone(),
                    format!("127.0.0.1:{port}").parse().unwrap(),
                    format!("127.0.0.1:{}", port + 1000).parse().unwrap(),
                    format!("127.0.0.1:{}", port + 2000).parse().unwrap(),
                    format!("127.0.0.1:{}", port + 3000).parse().unwrap(),
                    format!("127.0.0.1:{}", port + 4000).parse().unwrap(),
                    1,
                    1000,
                )),
                kp,
            )
        };

        crds.insert(make_ci(&kp_a, 8001)).unwrap();
        crds.insert(make_ci(&kp_b, 8002)).unwrap();

        let peers: Vec<(ContactInfo, u64)> =
            (0..5).map(|i| make_peer_with_stake(i, 1000)).collect();
        let peer_id = peers[0].0.identity();

        // Prune only kp_a's origin for peers[0].
        push.process_prune(peer_id, &[kp_a.address()], Duration::from_secs(60));

        let msgs = push.new_push_messages(&crds, &peers, &hash(b"seed"));

        // Find the message destined for peers[0].
        let peer_addr = peers[0].0.gossip_addr.0;
        if let Some((_, vals)) = msgs.iter().find(|(addr, _)| *addr == peer_addr) {
            // peers[0] must NOT receive kp_a's origin.
            assert!(
                vals.iter().all(|v| v.origin() != kp_a.address()),
                "pruned origin must not reach pruning peer"
            );
            // peers[0] MUST receive kp_b's origin (not pruned).
            assert!(
                vals.iter().any(|v| v.origin() == kp_b.address()),
                "non-pruned origin must reach pruning peer"
            );
        }
    }

    #[test]
    fn timed_prune_expiry() {
        let push = CrdsGossipPush::new(hash(b"me"));
        let pruner = hash(b"peer");
        let origin = hash(b"origin");

        // Prune with zero TTL (already expired).
        push.process_prune(pruner, &[origin], Duration::from_millis(0));
        push.purge_expired_prunes(Instant::now() + Duration::from_millis(1));

        assert!(
            push.prune_map.read().is_empty(),
            "expired prune must be purged"
        );
    }

    #[test]
    fn concurrent_insert_not_lost() {
        let kp_me = Keypair::generate();
        let my_identity = kp_me.address();
        let push = CrdsGossipPush::new(my_identity);
        let crds = CrdsTable::new();

        let kp_a = Keypair::generate();
        let ci_a = ContactInfo::new(
            kp_a.public_key().clone(),
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.1:9000".parse().unwrap(),
            "127.0.0.1:9100".parse().unwrap(),
            "127.0.0.1:9200".parse().unwrap(),
            "127.0.0.1:9300".parse().unwrap(),
            1,
            1000,
        );
        crds.insert(CrdsValue::new_signed(CrdsData::ContactInfo(ci_a), &kp_a))
            .unwrap();

        let peers: Vec<(ContactInfo, u64)> =
            (0..3).map(|i| make_peer_with_stake(i, 1000)).collect();
        let msgs = push.new_push_messages(&crds, &peers, &hash(b"seed1"));
        assert!(!msgs.is_empty());

        let kp_b = Keypair::generate();
        let ci_b = ContactInfo::new(
            kp_b.public_key().clone(),
            "127.0.0.2:8000".parse().unwrap(),
            "127.0.0.2:9000".parse().unwrap(),
            "127.0.0.2:9100".parse().unwrap(),
            "127.0.0.2:9200".parse().unwrap(),
            "127.0.0.2:9300".parse().unwrap(),
            1,
            2000,
        );
        crds.insert(CrdsValue::new_signed(CrdsData::ContactInfo(ci_b), &kp_b))
            .unwrap();

        let msgs2 = push.new_push_messages(&crds, &peers, &hash(b"seed2"));
        assert!(!msgs2.is_empty(), "value B must not be lost");

        let pushed_origins: Vec<Hash> = msgs2
            .iter()
            .flat_map(|(_, vals)| vals.iter().map(|v| v.origin()))
            .collect();
        assert!(
            pushed_origins.contains(&kp_b.address()),
            "value B's origin must be in the pushed values"
        );
    }
}
