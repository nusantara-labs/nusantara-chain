use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use nusantara_crypto::{Hash, Keypair, PublicKey, hashv};
use parking_lot::RwLock;

use std::time::{SystemTime, UNIX_EPOCH};

use crate::contact_info::ContactInfo;
use crate::crds::CrdsTable;
use crate::crds_gossip_pull::CrdsGossipPull;
use crate::crds_gossip_push::CrdsGossipPush;
use crate::crds_value::{CrdsData, CrdsValue, CrdsVote};
use crate::ping_pong::PingCache;
use crate::protocol::PruneMessage;

pub struct ClusterInfo {
    keypair: Arc<Keypair>,
    my_contact_info: RwLock<ContactInfo>,
    crds: Arc<CrdsTable>,
    push: CrdsGossipPush,
    pull: CrdsGossipPull,
    ping_cache: PingCache,
    entrypoints: Vec<SocketAddr>,
}

impl ClusterInfo {
    pub fn new(
        keypair: Arc<Keypair>,
        contact_info: ContactInfo,
        entrypoints: Vec<SocketAddr>,
        ping_cache_ttl_ms: u64,
    ) -> Self {
        let my_identity = keypair.address();
        let crds = Arc::new(CrdsTable::new());

        // Insert our own contact info
        let self_value = CrdsValue::new_signed(
            CrdsData::ContactInfo(contact_info.clone()),
            &keypair,
        );
        crds.insert(self_value).expect("self-insert cannot fail");

        Self {
            keypair,
            my_contact_info: RwLock::new(contact_info),
            crds,
            push: CrdsGossipPush::new(my_identity),
            pull: CrdsGossipPull::new(my_identity),
            ping_cache: PingCache::new(ping_cache_ttl_ms),
            entrypoints,
        }
    }

    pub fn my_identity(&self) -> Hash {
        self.keypair.address()
    }

    pub fn my_contact_info(&self) -> ContactInfo {
        self.my_contact_info.read().clone()
    }

    pub fn keypair(&self) -> &Arc<Keypair> {
        &self.keypair
    }

    pub fn crds(&self) -> &Arc<CrdsTable> {
        &self.crds
    }

    pub fn push(&self) -> &CrdsGossipPush {
        &self.push
    }

    pub fn pull(&self) -> &CrdsGossipPull {
        &self.pull
    }

    pub fn ping_cache(&self) -> &PingCache {
        &self.ping_cache
    }

    pub fn entrypoints(&self) -> &[SocketAddr] {
        &self.entrypoints
    }

    /// Get all known peers (excluding self).
    pub fn all_peers(&self) -> Vec<ContactInfo> {
        let my_id = self.my_identity();
        self.crds
            .all_contact_infos()
            .into_iter()
            .filter(|ci| ci.identity != my_id)
            .collect()
    }

    /// Get contact info for a specific validator.
    pub fn get_contact_info(&self, identity: &Hash) -> Option<ContactInfo> {
        self.crds.get_contact_info(identity)
    }

    /// Look up the public key for a validator identity from CRDS ContactInfo.
    pub fn get_pubkey(&self, identity: &Hash) -> Option<PublicKey> {
        self.crds
            .get_contact_info(identity)
            .map(|ci| ci.pubkey.clone())
    }

    /// Insert a CRDS value received from the network after verifying its signature.
    /// For ContactInfo, the pubkey is embedded; for other types, looks up the
    /// origin's pubkey from existing CRDS entries.
    ///
    /// Non-ContactInfo values from unknown peers are REJECTED (prevents accepting
    /// unverifiable data that could poison the CRDS table).
    pub fn insert_verified_crds_value(&self, value: CrdsValue) -> bool {
        let pubkey = match &value.data {
            CrdsData::ContactInfo(ci) => Some(ci.pubkey.clone()),
            _ => self.get_pubkey(&value.origin()),
        };

        match pubkey {
            Some(pk) => {
                if !value.verify(&pk) {
                    metrics::counter!("nusantara_gossip_invalid_signature_total").increment(1);
                    tracing::debug!(
                        origin = ?value.origin(),
                        label = %value.label(),
                        "dropping CRDS value with invalid signature"
                    );
                    return false;
                }
            }
            None => {
                // Non-ContactInfo from unknown peer — reject
                metrics::counter!("nusantara_gossip_unverifiable_value_dropped_total").increment(1);
                tracing::debug!(
                    origin = ?value.origin(),
                    label = %value.label(),
                    "rejecting unverifiable non-ContactInfo from unknown peer"
                );
                return false;
            }
        }

        self.crds.insert(value).is_ok()
    }

    /// Insert a CRDS value without verification (for backward compat in tests).
    pub fn insert_crds_value(&self, value: CrdsValue) -> bool {
        self.crds.insert(value).is_ok()
    }

    /// Update our contact info (e.g. new wallclock).
    pub fn update_self_contact_info(&self, contact_info: ContactInfo) {
        let value = CrdsValue::new_signed(
            CrdsData::ContactInfo(contact_info.clone()),
            &self.keypair,
        );
        let _ = self.crds.insert(value);
        *self.my_contact_info.write() = contact_info;
    }

    /// Refresh our own ContactInfo wallclock so it doesn't get purged.
    pub fn refresh_self_wallclock(&self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_millis() as u64;
        let mut ci = self.my_contact_info.write();
        ci.wallclock = now;
        let value = CrdsValue::new_signed(
            CrdsData::ContactInfo(ci.clone()),
            &self.keypair,
        );
        let _ = self.crds.insert(value);
    }

    /// Get peers with stakes for push/pull operations.
    /// Uses HashMap for O(1) stake lookups.
    pub fn peers_with_stakes(&self, stakes: &HashMap<Hash, u64>) -> Vec<(ContactInfo, u64)> {
        let peers = self.all_peers();
        peers
            .into_iter()
            .map(|ci| {
                let stake = stakes.get(&ci.identity).copied().unwrap_or(1);
                (ci, stake)
            })
            .collect()
    }

    pub fn peer_count(&self) -> usize {
        self.all_peers().len()
    }

    /// Publish a vote to gossip via CRDS.
    pub fn push_vote(&self, slot: u64, hash: Hash) {
        let wallclock = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_millis() as u64;

        let vote = CrdsVote {
            from: self.my_identity(),
            slot,
            hash,
            wallclock,
        };
        let value = CrdsValue::new_signed(CrdsData::Vote(vote), &self.keypair);
        let _ = self.crds.insert(value);
    }

    /// Get votes inserted since the given cursor.
    /// Returns `(votes, new_cursor)`.
    pub fn get_votes_since(&self, cursor: u64) -> (Vec<CrdsVote>, u64) {
        let new_cursor = self.crds.current_cursor();
        let values = self.crds.values_since(cursor);
        let votes = values
            .into_iter()
            .filter_map(|v| {
                if let CrdsData::Vote(vote) = v.data {
                    Some(vote)
                } else {
                    None
                }
            })
            .collect();
        (votes, new_cursor)
    }

    /// Create a signed prune message for sending to a peer.
    pub fn create_signed_prune_message(
        &self,
        prunes: Vec<Hash>,
        destination: Hash,
    ) -> PruneMessage {
        let wallclock = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_millis() as u64;

        let sign_data = hashv(&[
            b"prune",
            &borsh::to_vec(&prunes).unwrap_or_default(),
            destination.as_bytes(),
            &wallclock.to_le_bytes(),
        ]);
        let signature = self.keypair.sign(sign_data.as_bytes());

        PruneMessage {
            from: self.my_identity(),
            prunes,
            destination,
            wallclock,
            signature,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::Keypair;

    fn make_cluster_info() -> ClusterInfo {
        let kp = Arc::new(Keypair::generate());
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
        ClusterInfo::new(kp, ci, vec![], 60_000)
    }

    #[test]
    fn new_contains_self() {
        let ci = make_cluster_info();
        assert_eq!(ci.crds().len(), 1);
        assert!(ci.all_peers().is_empty()); // self excluded
    }

    #[test]
    fn insert_peer() {
        let ci = make_cluster_info();
        let other_kp = Keypair::generate();
        let other_ci = ContactInfo::new(
            other_kp.public_key().clone(),
            "127.0.0.1:9000".parse().unwrap(),
            "127.0.0.1:9003".parse().unwrap(),
            "127.0.0.1:9004".parse().unwrap(),
            "127.0.0.1:9001".parse().unwrap(),
            "127.0.0.1:9002".parse().unwrap(),
            1,
            1000,
        );
        let value = CrdsValue::new_signed(
            CrdsData::ContactInfo(other_ci),
            &other_kp,
        );

        assert!(ci.insert_crds_value(value));
        assert_eq!(ci.peer_count(), 1);
    }

    #[test]
    fn get_contact_info() {
        let ci = make_cluster_info();
        let my_id = ci.my_identity();
        assert!(ci.get_contact_info(&my_id).is_some());
    }

    #[test]
    fn get_pubkey() {
        let ci = make_cluster_info();
        let my_id = ci.my_identity();
        let pk = ci.get_pubkey(&my_id);
        assert!(pk.is_some());
    }

    #[test]
    fn verified_insert_accepts_valid() {
        let ci = make_cluster_info();
        let other_kp = Keypair::generate();
        let other_ci = ContactInfo::new(
            other_kp.public_key().clone(),
            "127.0.0.1:9000".parse().unwrap(),
            "127.0.0.1:9003".parse().unwrap(),
            "127.0.0.1:9004".parse().unwrap(),
            "127.0.0.1:9001".parse().unwrap(),
            "127.0.0.1:9002".parse().unwrap(),
            1,
            1000,
        );
        let value = CrdsValue::new_signed(
            CrdsData::ContactInfo(other_ci),
            &other_kp,
        );
        assert!(ci.insert_verified_crds_value(value));
        assert_eq!(ci.peer_count(), 1);
    }

    #[test]
    fn verified_insert_rejects_forged_contact_info() {
        let ci = make_cluster_info();
        let real_kp = Keypair::generate();
        let forger_kp = Keypair::generate();

        // Create ContactInfo for real_kp but sign with forger_kp
        let other_ci = ContactInfo::new(
            real_kp.public_key().clone(),
            "127.0.0.1:9000".parse().unwrap(),
            "127.0.0.1:9003".parse().unwrap(),
            "127.0.0.1:9004".parse().unwrap(),
            "127.0.0.1:9001".parse().unwrap(),
            "127.0.0.1:9002".parse().unwrap(),
            1,
            1000,
        );
        let forged = CrdsValue::new_signed(
            CrdsData::ContactInfo(other_ci),
            &forger_kp,
        );
        // Forged value should be rejected (pubkey in ContactInfo != signer)
        assert!(!ci.insert_verified_crds_value(forged));
        assert_eq!(ci.peer_count(), 0);
    }

    #[test]
    fn verified_insert_rejects_vote_from_unknown_peer() {
        let ci = make_cluster_info();
        let unknown_kp = Keypair::generate();

        // Vote from peer whose ContactInfo hasn't been inserted yet
        let vote = CrdsVote {
            from: unknown_kp.address(),
            slot: 1,
            hash: Hash::zero(),
            wallclock: 1000,
        };
        let value = CrdsValue::new_signed(CrdsData::Vote(vote), &unknown_kp);

        // Should be rejected: unknown peer, non-ContactInfo
        assert!(!ci.insert_verified_crds_value(value));
    }

    #[test]
    fn verified_insert_accepts_contact_info_from_unknown_peer() {
        let ci = make_cluster_info();
        let new_kp = Keypair::generate();

        // ContactInfo from new peer (always self-verifiable)
        let new_ci = ContactInfo::new(
            new_kp.public_key().clone(),
            "127.0.0.1:9000".parse().unwrap(),
            "127.0.0.1:9003".parse().unwrap(),
            "127.0.0.1:9004".parse().unwrap(),
            "127.0.0.1:9001".parse().unwrap(),
            "127.0.0.1:9002".parse().unwrap(),
            1,
            1000,
        );
        let value = CrdsValue::new_signed(CrdsData::ContactInfo(new_ci), &new_kp);
        assert!(ci.insert_verified_crds_value(value));
    }

    #[test]
    fn create_and_verify_prune_message() {
        let ci = make_cluster_info();
        let dest = Hash::zero();
        let prunes = vec![Hash::zero()];

        let prune = ci.create_signed_prune_message(prunes.clone(), dest);
        assert_eq!(prune.from, ci.my_identity());
        assert_eq!(prune.prunes, prunes);
        assert_eq!(prune.destination, dest);

        // Verify the signature
        let sign_data = hashv(&[
            b"prune",
            &borsh::to_vec(&prune.prunes).unwrap_or_default(),
            prune.destination.as_bytes(),
            &prune.wallclock.to_le_bytes(),
        ]);
        assert!(prune.signature.verify(ci.keypair().public_key(), sign_data.as_bytes()).is_ok());
    }
}
