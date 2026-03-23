use nusantara_consensus::commitment::CommitmentTracker;
use nusantara_consensus::fork_choice::ForkTree;
use nusantara_core::commitment::CommitmentLevel;
use nusantara_crypto::hash;

fn h(s: &str) -> nusantara_crypto::Hash {
    hash(s.as_bytes())
}

#[test]
fn test_fork_tree_heaviest_fork_with_stake() {
    let mut tree = ForkTree::new(0, h("b0"), h("bk0"));

    // Fork A: 0 -> 1 -> 2 -> 3
    tree.add_slot(1, 0, h("b1"), h("bk1")).unwrap();
    tree.add_slot(2, 1, h("b2"), h("bk2")).unwrap();
    tree.add_slot(3, 2, h("b3"), h("bk3")).unwrap();

    // Fork B: 0 -> 4 -> 5
    tree.add_slot(4, 0, h("b4"), h("bk4")).unwrap();
    tree.add_slot(5, 4, h("b5"), h("bk5")).unwrap();

    // Fork C: 0 -> 6
    tree.add_slot(6, 0, h("b6"), h("bk6")).unwrap();

    // Add stake-weighted votes
    tree.add_vote(3, 100); // Fork A gets 100 stake
    tree.add_vote(5, 150); // Fork B gets 150 stake
    tree.add_vote(6, 50); // Fork C gets 50 stake

    // Fork B should be the heaviest
    let best = tree.compute_best_fork();
    assert_eq!(best, 5);
}

#[test]
fn test_fork_tree_root_pruning() {
    let mut tree = ForkTree::new(0, h("b0"), h("bk0"));

    // Build: 0 -> 1 -> 2 -> 3 -> 4
    for slot in 1..=4 {
        tree.add_slot(
            slot,
            slot - 1,
            h(&format!("b{slot}")),
            h(&format!("bk{slot}")),
        )
        .unwrap();
    }
    // Branch: 0 -> 5
    tree.add_slot(5, 0, h("b5"), h("bk5")).unwrap();
    // Branch: 1 -> 6
    tree.add_slot(6, 1, h("b6"), h("bk6")).unwrap();

    assert_eq!(tree.node_count(), 7);

    // Set root to slot 2 - should prune slots 0, 1, 5, 6
    let pruned = tree.set_root(2);
    assert!(pruned.contains(&0));
    assert!(pruned.contains(&1));
    assert!(pruned.contains(&5));
    assert!(pruned.contains(&6));
    assert!(tree.contains(2));
    assert!(tree.contains(3));
    assert!(tree.contains(4));
    assert_eq!(tree.root_slot(), 2);
}

#[test]
fn test_fork_tree_reorg() {
    let mut tree = ForkTree::new(0, h("b0"), h("bk0"));

    // Fork A: 0 -> 1
    tree.add_slot(1, 0, h("b1"), h("bk1")).unwrap();
    // Fork B: 0 -> 2
    tree.add_slot(2, 0, h("b2"), h("bk2")).unwrap();

    // Initially fork A is best
    tree.add_vote(1, 100);
    assert_eq!(tree.compute_best_fork(), 1);

    // Fork B overtakes with more stake
    tree.add_vote(2, 200);
    assert_eq!(tree.compute_best_fork(), 2);

    // Add even more to fork A
    tree.add_vote(1, 250);
    assert_eq!(tree.compute_best_fork(), 1);
}

#[test]
fn test_fork_tree_with_commitment_tracker() {
    let total_stake = 1000;
    let mut tree = ForkTree::new(0, h("b0"), h("bk0"));
    let mut tracker = CommitmentTracker::new(total_stake);

    // Build chain
    tree.add_slot(1, 0, h("b1"), h("bk1")).unwrap();
    tree.add_slot(2, 1, h("b2"), h("bk2")).unwrap();

    // Track slots
    tracker.track_slot(1, h("b1"));
    tracker.track_slot(2, h("b2"));

    // Vote with 50% stake - not enough for confirmation
    tree.add_vote(1, 500);
    tracker.record_vote(1, h("b1"), 500);
    assert_eq!(
        tracker.get_commitment(1).unwrap(),
        CommitmentLevel::Processed
    );

    // Add more votes reaching 66% threshold
    tree.add_vote(1, 170);
    let level = tracker.record_vote(1, h("b1"), 170);
    assert_eq!(level, CommitmentLevel::Confirmed);

    // Finalize
    tracker.mark_finalized(1);
    assert_eq!(
        tracker.get_commitment(1).unwrap(),
        CommitmentLevel::Finalized
    );
}

#[test]
fn test_fork_tree_orphan_handling() {
    let mut tree = ForkTree::new(0, h("b0"), h("bk0"));

    // Add slot 3 whose parent (slot 2) doesn't exist yet
    tree.add_slot(3, 2, h("b3"), h("bk3")).unwrap();
    assert!(!tree.get_node(3).unwrap().is_connected);

    // Add slot 2 whose parent (slot 1) doesn't exist yet
    tree.add_slot(2, 1, h("b2"), h("bk2")).unwrap();

    // Add slot 1 connected to root
    tree.add_slot(1, 0, h("b1"), h("bk1")).unwrap();
    assert!(tree.get_node(1).unwrap().is_connected);

    // Reconnect orphans
    tree.try_reconnect_orphans();

    assert!(tree.get_node(2).unwrap().is_connected);
    assert!(tree.get_node(3).unwrap().is_connected);
}

#[test]
fn test_fork_tree_deep_chain() {
    let mut tree = ForkTree::new(0, h("b0"), h("bk0"));

    // Build a chain of 100 slots
    for slot in 1..=100 {
        tree.add_slot(
            slot,
            slot - 1,
            h(&format!("b{slot}")),
            h(&format!("bk{slot}")),
        )
        .unwrap();
    }

    assert_eq!(tree.node_count(), 101);

    // Ancestry from tip to root
    let ancestry = tree.get_ancestry(100);
    assert_eq!(ancestry.len(), 101);
    assert_eq!(*ancestry.first().unwrap(), 100);
    assert_eq!(*ancestry.last().unwrap(), 0);
}

#[test]
fn test_fork_tree_multiple_forks_and_votes() {
    let mut tree = ForkTree::new(0, h("b0"), h("bk0"));

    // 5 forks from root
    for fork in 1..=5 {
        let slot = fork * 100;
        tree.add_slot(slot, 0, h(&format!("bf{fork}")), h(&format!("bkf{fork}")))
            .unwrap();
        // Each fork has 3 more slots
        for depth in 1..=3 {
            tree.add_slot(
                slot + depth,
                slot + depth - 1,
                h(&format!("bf{fork}d{depth}")),
                h(&format!("bkf{fork}d{depth}")),
            )
            .unwrap();
        }
    }

    // Vote for fork 3 with the most stake
    tree.add_vote(303, 500);
    tree.add_vote(103, 100);
    tree.add_vote(203, 200);
    tree.add_vote(403, 300);
    tree.add_vote(503, 400);

    let best = tree.compute_best_fork();
    assert_eq!(best, 303); // Fork 3 has the most stake
}
