use std::sync::Arc;

use nusantara_consensus::bank::ConsensusBank;
use nusantara_consensus::commitment::CommitmentTracker;
use nusantara_consensus::fork_choice::ForkTree;
use nusantara_consensus::replay_stage::ReplayStage;
use nusantara_consensus::tower::Tower;
use nusantara_core::block::{Block, BlockHeader};
use nusantara_core::epoch::EpochSchedule;
use nusantara_crypto::{Hash, hash};
use nusantara_stake_program::Delegation;
use nusantara_storage::Storage;
use nusantara_vote_program::{VoteInit, VoteState};

fn setup_pipeline() -> (ReplayStage, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let epoch_schedule = EpochSchedule::new(100);
    let bank = Arc::new(ConsensusBank::new(storage, epoch_schedule));

    // Set up genesis state with vote accounts and stake
    let validator = hash(b"validator");
    let vote_account = hash(b"vote_account");

    let vs = VoteState::new(&VoteInit {
        node_pubkey: validator,
        authorized_voter: validator,
        authorized_withdrawer: validator,
        commission: 10,
    });
    bank.set_vote_state(vote_account, vs);

    bank.set_stake_delegation(
        hash(b"stake1"),
        Delegation {
            voter_pubkey: validator,
            stake: 1_000_000_000,
            activation_epoch: 0,
            deactivation_epoch: u64::MAX,
            warmup_cooldown_rate_bps: 2500,
        },
    );

    bank.recalculate_epoch_stakes(0);

    let init = VoteInit {
        node_pubkey: validator,
        authorized_voter: validator,
        authorized_withdrawer: validator,
        commission: 10,
    };
    let tower = Tower::new(VoteState::new(&init));
    let fork_tree = ForkTree::new(0, hash(b"genesis"), hash(b"genesis_bank"));
    let commitment = CommitmentTracker::new(bank.total_active_stake());

    let stage = ReplayStage::new(validator, bank, tower, fork_tree, commitment, None);
    (stage, dir)
}

fn make_block(slot: u64, parent_slot: u64, validator: Hash) -> Block {
    Block {
        header: BlockHeader {
            slot,
            parent_slot,
            parent_hash: hash(format!("parent_{parent_slot}").as_bytes()),
            block_hash: hash(format!("block_{slot}").as_bytes()),
            timestamp: 1000 + slot as i64,
            validator,
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

#[test]
fn test_full_consensus_pipeline() {
    let (mut stage, _dir) = setup_pipeline();
    let validator = hash(b"validator");

    // Produce and replay 10 blocks
    for slot in 1..=10 {
        let block = make_block(slot, slot - 1, validator);
        let result = stage.replay_block(&block, &[]).unwrap();
        assert_eq!(result.slot, slot);
        assert_eq!(result.parent_slot, slot - 1);
    }

    // Verify fork tree state
    assert_eq!(stage.fork_tree().node_count(), 11); // genesis + 10

    // Verify bank state updated
    assert_eq!(stage.bank().current_slot(), 10);

    // Verify slot hashes recorded
    let slot_hashes = stage.bank().slot_hashes();
    for slot in 1..=10 {
        assert!(slot_hashes.get(slot).is_some());
    }

    // Verify storage persisted
    for slot in 1..=10 {
        let bank_hash = stage.bank().storage().get_bank_hash(slot).unwrap();
        assert!(bank_hash.is_some());
    }
}

#[test]
fn test_consensus_pipeline_with_forks() {
    let (mut stage, _dir) = setup_pipeline();
    let validator = hash(b"validator");

    // Build main chain: 0 -> 1 -> 2 -> 3
    for slot in 1..=3 {
        let block = make_block(slot, slot - 1, validator);
        stage.replay_block(&block, &[]).unwrap();
    }

    // Build fork: 0 -> 4 -> 5
    let fork_block_4 = make_block(4, 0, validator);
    stage.replay_block(&fork_block_4, &[]).unwrap();
    let fork_block_5 = make_block(5, 4, validator);
    stage.replay_block(&fork_block_5, &[]).unwrap();

    // Both forks should exist
    assert!(stage.fork_tree().contains(3));
    assert!(stage.fork_tree().contains(5));
    assert_eq!(stage.fork_tree().node_count(), 6);

    // Continue building main chain from slot 3
    stage
        .replay_block(&make_block(6, 3, validator), &[])
        .unwrap();
    stage
        .replay_block(&make_block(7, 6, validator), &[])
        .unwrap();
    stage
        .replay_block(&make_block(8, 7, validator), &[])
        .unwrap();

    assert!(stage.fork_tree().contains(8));
}

#[test]
fn test_consensus_pipeline_leader_schedule() {
    let (mut stage, _dir) = setup_pipeline();
    let validator = hash(b"validator");
    let seed = hash(b"epoch_seed");

    // Compute leader schedule and clone to avoid borrow conflict
    let es = stage.bank().epoch_schedule().clone();
    let schedule = stage.get_leader_schedule(0, &seed).unwrap().clone();

    // With only one validator, they should be the leader for all slots
    for slot in 0..10 {
        let leader = schedule.get_leader(slot, &es);
        assert!(leader.is_some());
        assert_eq!(*leader.unwrap(), validator);
    }
}

#[test]
fn test_consensus_pipeline_shutdown() {
    let (mut stage, _dir) = setup_pipeline();
    let validator = hash(b"validator");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let (tx, rx) = tokio::sync::mpsc::channel(10);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        // Send a few blocks
        for slot in 1..=3 {
            let block = make_block(slot, slot - 1, validator);
            tx.send((block, Vec::new())).await.unwrap();
        }

        // Drop sender to signal no more blocks, then shutdown
        drop(tx);
        shutdown_tx.send(true).unwrap();

        // Run replay stage - should process available blocks and then shutdown
        stage.run(rx, shutdown_rx).await;

        // At least some blocks should have been processed
        assert!(stage.bank().current_slot() >= 1);
    });
}

#[test]
fn test_consensus_pipeline_storage_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let validator = hash(b"validator");

    // First session: create and process blocks
    {
        let storage = Arc::new(Storage::open(dir.path()).unwrap());
        let bank = Arc::new(ConsensusBank::new(storage, EpochSchedule::new(100)));
        let init = VoteInit {
            node_pubkey: validator,
            authorized_voter: validator,
            authorized_withdrawer: validator,
            commission: 10,
        };
        let tower = Tower::new(VoteState::new(&init));
        let fork_tree = ForkTree::new(0, hash(b"genesis"), hash(b"genesis_bank"));
        let commitment = CommitmentTracker::new(0);
        let mut stage = ReplayStage::new(validator, bank, tower, fork_tree, commitment, None);

        for slot in 1..=5 {
            let block = make_block(slot, slot - 1, validator);
            stage.replay_block(&block, &[]).unwrap();
        }
    }

    // Second session: verify data persisted
    {
        let storage = Storage::open(dir.path()).unwrap();
        for slot in 1..=5 {
            let bank_hash = storage.get_bank_hash(slot).unwrap();
            assert!(bank_hash.is_some(), "Bank hash missing for slot {slot}");

            let slot_hash = storage.get_slot_hash(slot).unwrap();
            assert!(slot_hash.is_some(), "Slot hash missing for slot {slot}");
        }
    }
}
