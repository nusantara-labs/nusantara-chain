use nusantara_crypto::Hash;
use nusantara_vote_program::VoteState;
use tracing::instrument;

use crate::bank::ConsensusBank;

impl ConsensusBank {
    /// Register a vote account.
    #[instrument(skip(self, state), fields(vote_account = %vote_account), level = "debug")]
    pub fn set_vote_state(&self, vote_account: Hash, state: VoteState) {
        self.vote_accounts.insert(vote_account, state);
    }

    /// Get vote state for a vote account.
    #[instrument(skip(self), fields(vote_account = %vote_account), level = "debug")]
    pub fn get_vote_state(&self, vote_account: &Hash) -> Option<VoteState> {
        self.vote_accounts.get(vote_account).map(|v| v.clone())
    }

    /// Update vote state after processing a vote.
    #[instrument(skip(self, vote_state), fields(vote_account = %vote_account), level = "debug")]
    pub fn update_vote_state(&self, vote_account: &Hash, vote_state: VoteState) {
        self.vote_accounts.insert(*vote_account, vote_state);
    }

    /// Get all vote states.
    #[instrument(skip(self), level = "debug")]
    pub fn get_all_vote_states(&self) -> Vec<(Hash, VoteState)> {
        self.vote_accounts
            .iter()
            .map(|entry| (*entry.key(), entry.value().clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::test_utils::test_helpers::temp_bank;

    #[test]
    fn vote_state_crud() {
        let (bank, _storage, _dir) = temp_bank();

        let addr = nusantara_crypto::hash(b"vote_acc");
        let vs = nusantara_vote_program::VoteState::new(&nusantara_vote_program::VoteInit {
            node_pubkey: nusantara_crypto::hash(b"node"),
            authorized_voter: nusantara_crypto::hash(b"voter"),
            authorized_withdrawer: nusantara_crypto::hash(b"wd"),
            commission: 10,
        });

        assert!(bank.get_vote_state(&addr).is_none());
        bank.set_vote_state(addr, vs.clone());
        assert_eq!(bank.get_vote_state(&addr).unwrap(), vs);
    }
}
