use nusantara_crypto::Hash;
use nusantara_vote_program::{Vote, VoteInstruction};

use crate::replay_stage::ReplayStage;

/// Maximum number of pending vote entries (slot-level) to buffer before
/// evicting the oldest ones. Prevents unbounded memory growth if gossip
/// delivers votes far ahead of the fork tree.
const MAX_PENDING_VOTE_SLOTS: usize = 10_000;

impl ReplayStage {
    /// Process a gossip vote from a peer validator.
    ///
    /// If the voted slot is not yet in the fork tree the vote is buffered in
    /// `pending_votes` so it can be replayed once the slot is added.
    pub fn process_gossip_vote(&mut self, _voter: Hash, slot: u64, hash: Hash, stake: u64) {
        if self.fork_tree.contains(slot) {
            self.fork_tree.add_vote(slot, stake);
            self.commitment_tracker.record_vote(slot, hash, stake);
        } else {
            // Buffer the vote for later replay once the slot enters the tree.
            let pending = self.pending_votes.entry(slot).or_default();
            pending.push((hash, stake));

            // Evict oldest slots when the map exceeds capacity.
            if self.pending_votes.len() > MAX_PENDING_VOTE_SLOTS {
                let mut slots: Vec<u64> = self.pending_votes.keys().copied().collect();
                slots.sort_unstable();
                let to_remove = self.pending_votes.len() - MAX_PENDING_VOTE_SLOTS;
                for &s in &slots[..to_remove] {
                    self.pending_votes.remove(&s);
                }
            }
        }
    }

    /// Drain any pending gossip votes for `slot` and apply them to the fork
    /// tree and commitment tracker. Called after a slot is added to the tree.
    pub(crate) fn drain_pending_votes(&mut self, slot: u64) {
        if let Some(votes) = self.pending_votes.remove(&slot) {
            for (hash, stake) in votes {
                self.fork_tree.add_vote(slot, stake);
                self.commitment_tracker.record_vote(slot, hash, stake);
            }
        }
    }
}

/// Extract a Vote and the voter's identity from a transaction.
///
/// The voter identity is the **authorized_voter** — the second account in the
/// vote instruction's account list (`ix.accounts[1]`), NOT the fee payer
/// (`account_keys[0]`). The fee payer may differ from the authorized voter
/// when a separate relayer pays for the transaction.
pub(crate) fn extract_vote_from_transaction(
    tx: &nusantara_core::transaction::Transaction,
) -> Option<(Hash, Vote)> {
    use nusantara_core::program::VOTE_PROGRAM_ID;

    for ix in &tx.message.instructions {
        let program_id = tx.message.account_keys.get(ix.program_id_index as usize)?;
        if *program_id == *VOTE_PROGRAM_ID
            && let Ok(vote_ix) = borsh::from_slice::<VoteInstruction>(&ix.data)
        {
            // The vote instruction layout (see vote-program/src/lib.rs):
            //   accounts[0] = vote_account (writable)
            //   accounts[1] = authorized_voter (signer, readonly)
            let voter_index = *ix.accounts.get(1)? as usize;
            let voter = *tx.message.account_keys.get(voter_index)?;

            match vote_ix {
                VoteInstruction::Vote(vote) => return Some((voter, vote)),
                VoteInstruction::SwitchVote(vote, _) => return Some((voter, vote)),
                _ => {}
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_core::instruction::{AccountMeta, Instruction};
    use nusantara_core::message::Message;
    use nusantara_core::program::VOTE_PROGRAM_ID;
    use nusantara_core::transaction::Transaction;
    use nusantara_crypto::{Keypair, hash};

    use crate::test_utils::test_helpers::make_replay_stage;

    /// Build a vote transaction where the fee payer differs from the
    /// authorized_voter, verifying that the extractor returns the correct
    /// voter identity.
    #[test]
    fn extract_voter_is_authorized_voter_not_fee_payer() {
        let payer_kp = Keypair::generate();
        let payer = payer_kp.address();

        let voter_kp = Keypair::generate();
        let voter = voter_kp.address();

        let vote_account = hash(b"vote_account");

        // Ensure fee payer != authorized voter
        assert_ne!(payer, voter);

        let vote = Vote {
            slots: vec![42],
            hash: hash(b"blockhash"),
            timestamp: None,
        };
        let ix = Instruction {
            program_id: *VOTE_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(vote_account, false),
                AccountMeta::new_readonly(voter, true),
            ],
            data: borsh::to_vec(&VoteInstruction::Vote(vote.clone())).unwrap(),
        };

        let msg = Message::new(&[ix], &payer).unwrap();
        let mut tx = Transaction::new(msg);
        tx.sign(&[&payer_kp, &voter_kp]);

        let (extracted_voter, extracted_vote) =
            extract_vote_from_transaction(&tx).expect("should extract vote");

        // The voter must be the authorized_voter, not the fee payer
        assert_eq!(extracted_voter, voter);
        assert_ne!(extracted_voter, payer);
        assert_eq!(extracted_vote.slots, vote.slots);
    }

    /// Verify that votes for unknown slots are buffered and later drained.
    #[test]
    fn pending_votes_buffered_and_drained() {
        let (mut stage, _dir) = make_replay_stage();
        let block_hash = hash(b"block_5");

        // Slot 5 is NOT in the fork tree yet
        assert!(!stage.fork_tree.contains(5));

        // Process a gossip vote for slot 5 — should be buffered
        stage.process_gossip_vote(hash(b"voter"), 5, block_hash, 100);
        assert!(stage.pending_votes.contains_key(&5));
        // Fork tree should NOT have any vote weight for slot 5
        assert!(stage.fork_tree.get_node(5).is_none());

        // Now add slot 5 to the fork tree
        stage
            .fork_tree
            .add_slot(5, 0, block_hash, hash(b"bank_5"))
            .unwrap();

        // Drain pending votes
        stage.drain_pending_votes(5);

        // Pending should be empty
        assert!(!stage.pending_votes.contains_key(&5));

        // The vote should now be recorded in the fork tree
        let node = stage.fork_tree.get_node(5).unwrap();
        assert_eq!(node.stake_voted, 100);
    }
}
