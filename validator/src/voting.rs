use nusantara_crypto::Hash;
use nusantara_vote_program::Vote;
use tracing::{info, warn};

use crate::constants::{MAX_VOTE_BATCH, SLASH_PURGE_DEPTH, SLASH_PURGE_INTERVAL};
use crate::helpers;
use crate::node::ValidatorNode;
use crate::vote_tx::build_vote_transaction;

impl ValidatorNode {
    /// Submit a single vote transaction covering all unvoted slots since the
    /// last vote. This batches `[last_voted_slot+1 ..= slot]` into one Vote
    /// instead of emitting one tx per slot, which eliminates the burst of
    /// VoteTooOld / LockoutViolation errors on replaying validators.
    pub(crate) fn submit_vote(&mut self, slot: u64) {
        let Some(vote_account) = self.my_vote_account else {
            return;
        };

        if slot <= self.last_voted_slot {
            return;
        }

        // Collect unvoted slots, capped to MAX_VOTE_BATCH
        let start = slot
            .saturating_sub(MAX_VOTE_BATCH - 1)
            .max(self.last_voted_slot + 1);
        let vote_slots: Vec<u64> = (start..=slot).collect();

        let block_hash = self
            .bank
            .slot_hashes()
            .0
            .iter()
            .find(|(s, _)| *s == slot)
            .map(|(_, h)| *h)
            .unwrap_or(Hash::zero());

        let timestamp = helpers::unix_timestamp_secs();

        let vote = Vote {
            slots: vote_slots,
            hash: block_hash,
            timestamp: Some(timestamp),
        };

        let tx = build_vote_transaction(&self.keypair, &vote_account, vote, block_hash);
        let _ = self.mempool.insert(tx); // best-effort

        self.last_voted_slot = slot;

        // Also publish vote via gossip for fast propagation (F4)
        self.cluster_info.push_vote(slot, block_hash);

        metrics::counter!("nusantara_votes_submitted").increment(1);
    }

    /// Drain gossip votes and feed them into the consensus engine.
    ///
    /// Before normal processing, each vote is checked for equivocation (double-voting).
    /// If a validator voted for two different blocks at the same slot, a `SlashProof`
    /// is persisted and a 5% stake penalty is applied to the slash registry.
    pub(crate) fn process_gossip_votes(&mut self) {
        let (votes, new_cursor) = self.cluster_info.get_votes_since(self.gossip_vote_cursor);
        for vote in &votes {
            // Check for equivocation before processing the vote normally
            if let Some(proof) = self.slash_detector.check_vote(
                &vote.from,
                vote.slot,
                &vote.hash,
                &self.identity,
            ) {
                // Persist slash proof to storage
                if let Err(e) = self.storage.put_slash_proof(&proof) {
                    warn!(error = %e, "failed to store slash proof");
                }

                // Calculate penalty: 5% of validator's current effective stake
                let validator_stake = self.bank.get_validator_stake(&proof.validator);
                let penalty =
                    validator_stake * nusantara_consensus::SLASH_PENALTY_BPS / 10_000;
                if penalty > 0 {
                    self.bank.apply_slash(&proof.validator, penalty);
                    info!(
                        validator = %proof.validator.to_base64(),
                        slot = proof.slot,
                        penalty,
                        "slash penalty applied for double vote"
                    );
                }
            }

            // Normal vote processing
            let stake = self.bank.get_validator_stake(&vote.from);
            if stake > 0 {
                self.replay_stage
                    .process_gossip_vote(vote.from, vote.slot, vote.hash, stake);
            }
        }

        // Periodic purge of old slash detector entries to bound memory
        if self.current_slot.is_multiple_of(SLASH_PURGE_INTERVAL) {
            self.slash_detector
                .purge_below(self.current_slot.saturating_sub(SLASH_PURGE_DEPTH));
        }

        if !votes.is_empty() {
            metrics::counter!("nusantara_gossip_votes_processed").increment(votes.len() as u64);
        }
        self.gossip_vote_cursor = new_cursor;
    }
}
