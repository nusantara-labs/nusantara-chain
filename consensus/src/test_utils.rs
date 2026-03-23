#[cfg(test)]
pub(crate) mod test_helpers {
    use std::sync::Arc;

    use nusantara_core::block::{Block, BlockHeader};
    use nusantara_core::epoch::EpochSchedule;
    use nusantara_crypto::{Hash, hash};
    use nusantara_storage::Storage;
    use nusantara_vote_program::{VoteInit, VoteState};

    use crate::bank::ConsensusBank;
    use crate::commitment::CommitmentTracker;
    use crate::fork_choice::ForkTree;
    use crate::replay_stage::ReplayStage;
    use crate::tower::Tower;

    /// Create a temporary `ConsensusBank` backed by a temp directory.
    pub(crate) fn temp_bank() -> (ConsensusBank, Arc<Storage>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open(dir.path()).unwrap());
        let bank = ConsensusBank::new(Arc::clone(&storage), EpochSchedule::new(100));
        (bank, storage, dir)
    }

    /// Create a `ReplayStage` with default test configuration.
    pub(crate) fn make_replay_stage() -> (ReplayStage, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open(dir.path()).unwrap());
        let epoch_schedule = EpochSchedule::new(100);
        let bank = Arc::new(ConsensusBank::new(storage, epoch_schedule));

        let init = VoteInit {
            node_pubkey: hash(b"node"),
            authorized_voter: hash(b"voter"),
            authorized_withdrawer: hash(b"wd"),
            commission: 10,
        };
        let tower = Tower::new(VoteState::new(&init));
        let fork_tree = ForkTree::new(0, hash(b"genesis"), hash(b"genesis_bank"));
        let commitment = CommitmentTracker::new(1000);

        let stage = ReplayStage::new(hash(b"node"), bank, tower, fork_tree, commitment, None);
        (stage, dir)
    }

    /// Create a simple test block with the given slot and parent.
    pub(crate) fn make_block(slot: u64, parent_slot: u64) -> Block {
        Block {
            header: BlockHeader {
                slot,
                parent_slot,
                parent_hash: hash(format!("parent_{parent_slot}").as_bytes()),
                block_hash: hash(format!("block_{slot}").as_bytes()),
                timestamp: 1000 + slot as i64,
                validator: hash(b"validator"),
                transaction_count: 0,
                merkle_root: Hash::zero(),
                poh_hash: Hash::zero(),
                bank_hash: Hash::zero(),
                state_root: Hash::zero(),
            },
            transactions: Vec::new(),
            batches: Vec::new(),
        }
    }

    /// Builder for creating `ConsensusBank` instances with custom configuration.
    #[allow(dead_code)]
    pub(crate) struct TestBankBuilder {
        slots_per_epoch: u64,
    }

    #[allow(dead_code)]
    impl TestBankBuilder {
        pub(crate) fn new() -> Self {
            Self {
                slots_per_epoch: 100,
            }
        }

        pub(crate) fn slots_per_epoch(mut self, slots: u64) -> Self {
            self.slots_per_epoch = slots;
            self
        }

        pub(crate) fn build(self) -> (ConsensusBank, Arc<Storage>, tempfile::TempDir) {
            let dir = tempfile::tempdir().unwrap();
            let storage = Arc::new(Storage::open(dir.path()).unwrap());
            let bank = ConsensusBank::new(
                Arc::clone(&storage),
                EpochSchedule::new(self.slots_per_epoch),
            );
            (bank, storage, dir)
        }
    }
}
