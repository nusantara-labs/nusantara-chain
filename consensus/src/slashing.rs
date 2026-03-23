use dashmap::DashMap;
use nusantara_crypto::Hash;
use nusantara_storage::SlashProof;
use tracing::info;

/// Default slash penalty: 5% of delegated stake per equivocation (500 basis points).
pub const SLASH_PENALTY_BPS: u64 = 500;

/// Detects double-voting (equivocation) from gossip votes.
///
/// Tracks the first vote seen per (validator, slot) pair. If a second vote arrives
/// for the same (validator, slot) but a different block hash, a `SlashProof` is produced.
///
/// Old entries are periodically purged via `purge_below` to prevent unbounded growth.
pub struct SlashDetector {
    /// (validator, slot) -> first block_hash seen for that (validator, slot) pair.
    seen_votes: DashMap<(Hash, u64), Hash>,
}

impl SlashDetector {
    pub fn new() -> Self {
        Self {
            seen_votes: DashMap::new(),
        }
    }

    /// Check a vote for equivocation.
    ///
    /// Returns `Some(SlashProof)` if this vote conflicts with a previously observed
    /// vote from the same validator for the same slot (different block hash).
    /// Returns `None` if this is the first vote or a duplicate of the same vote.
    pub fn check_vote(
        &self,
        validator: &Hash,
        slot: u64,
        hash: &Hash,
        reporter: &Hash,
    ) -> Option<SlashProof> {
        let key = (*validator, slot);

        match self.seen_votes.entry(key) {
            dashmap::mapref::entry::Entry::Occupied(entry) => {
                let first_hash = *entry.get();
                if first_hash != *hash {
                    // Double vote detected
                    let timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("system clock before UNIX epoch")
                        .as_secs() as i64;

                    let proof = SlashProof {
                        validator: *validator,
                        slot,
                        vote1_hash: first_hash,
                        vote2_hash: *hash,
                        reporter: *reporter,
                        timestamp,
                    };

                    info!(
                        validator = %validator.to_base64(),
                        slot,
                        "double vote detected"
                    );
                    metrics::counter!("nusantara_slashing_double_votes_detected").increment(1);

                    Some(proof)
                } else {
                    // Same vote repeated, not equivocation
                    None
                }
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(*hash);
                None
            }
        }
    }

    /// Purge entries for slots below `min_slot` to bound memory usage.
    pub fn purge_below(&self, min_slot: u64) {
        self.seen_votes
            .retain(|(_validator, slot), _| *slot >= min_slot);
    }

    /// Number of tracked (validator, slot) pairs.
    pub fn len(&self) -> usize {
        self.seen_votes.len()
    }

    /// Whether the detector has no tracked votes.
    pub fn is_empty(&self) -> bool {
        self.seen_votes.is_empty()
    }
}

impl Default for SlashDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    #[test]
    fn honest_validator_no_slash() {
        let detector = SlashDetector::new();
        let validator = hash(b"honest");
        let reporter = hash(b"reporter");

        // Each slot gets a unique vote -- no equivocation
        for slot in 0..10 {
            let block_hash = hash(format!("block_{slot}").as_bytes());
            let result = detector.check_vote(&validator, slot, &block_hash, &reporter);
            assert!(
                result.is_none(),
                "honest vote at slot {slot} should not be slashed"
            );
        }
    }

    #[test]
    fn double_vote_detected() {
        let detector = SlashDetector::new();
        let validator = hash(b"cheater");
        let reporter = hash(b"reporter");

        let hash_a = hash(b"block_a");
        let hash_b = hash(b"block_b");

        // First vote at slot 5
        assert!(
            detector
                .check_vote(&validator, 5, &hash_a, &reporter)
                .is_none()
        );

        // Conflicting vote at slot 5
        let proof = detector
            .check_vote(&validator, 5, &hash_b, &reporter)
            .expect("should detect double vote");

        assert_eq!(proof.validator, validator);
        assert_eq!(proof.slot, 5);
        assert_eq!(proof.vote1_hash, hash_a);
        assert_eq!(proof.vote2_hash, hash_b);
        assert_eq!(proof.reporter, reporter);
    }

    #[test]
    fn same_vote_twice_no_slash() {
        let detector = SlashDetector::new();
        let validator = hash(b"repeat_voter");
        let reporter = hash(b"reporter");
        let block_hash = hash(b"same_block");

        // Vote once
        assert!(
            detector
                .check_vote(&validator, 10, &block_hash, &reporter)
                .is_none()
        );

        // Same vote again -- should not be flagged
        assert!(
            detector
                .check_vote(&validator, 10, &block_hash, &reporter)
                .is_none()
        );
    }

    #[test]
    fn different_validators_independent() {
        let detector = SlashDetector::new();
        let reporter = hash(b"reporter");

        let val_a = hash(b"validator_a");
        let val_b = hash(b"validator_b");
        let hash_a = hash(b"block_a");
        let hash_b = hash(b"block_b");

        // Both validators vote for different blocks at the same slot -- no slash
        // because they are different validators.
        assert!(detector.check_vote(&val_a, 5, &hash_a, &reporter).is_none());
        assert!(detector.check_vote(&val_b, 5, &hash_b, &reporter).is_none());
    }

    #[test]
    fn purge_old_entries() {
        let detector = SlashDetector::new();
        let validator = hash(b"val");
        let reporter = hash(b"rep");

        for slot in 0..100 {
            let h = hash(format!("block_{slot}").as_bytes());
            detector.check_vote(&validator, slot, &h, &reporter);
        }
        assert_eq!(detector.len(), 100);

        // Purge slots below 50
        detector.purge_below(50);
        assert_eq!(detector.len(), 50);

        // Old slot should accept a "new" vote (entry was purged)
        let h = hash(b"new_block_for_old_slot");
        assert!(detector.check_vote(&validator, 0, &h, &reporter).is_none());
    }

    #[test]
    fn penalty_constant() {
        assert_eq!(SLASH_PENALTY_BPS, 500);
        // 500 bps = 5%
        let stake: u64 = 1_000_000_000;
        let penalty = stake * SLASH_PENALTY_BPS / 10_000;
        assert_eq!(penalty, 50_000_000);
    }
}
