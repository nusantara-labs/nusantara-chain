use nusantara_consensus::tower::{MAX_LOCKOUT_HISTORY, Tower};
use nusantara_crypto::hash;
use nusantara_vote_program::{Vote, VoteInit, VoteState};

fn make_tower() -> Tower {
    let init = VoteInit {
        node_pubkey: hash(b"node"),
        authorized_voter: hash(b"voter"),
        authorized_withdrawer: hash(b"withdrawer"),
        commission: 10,
    };
    Tower::new(VoteState::new(&init))
}

fn make_vote(slot: u64) -> Vote {
    Vote {
        slots: vec![slot],
        hash: hash(slot.to_le_bytes().as_ref()),
        timestamp: None,
    }
}

#[test]
fn test_tower_31_vote_root_advancement() {
    let mut tower = make_tower();

    // Process MAX_LOCKOUT_HISTORY sequential votes
    for slot in 1..=MAX_LOCKOUT_HISTORY {
        let result = tower.process_vote(&make_vote(slot)).unwrap();

        if slot < MAX_LOCKOUT_HISTORY {
            assert!(result.new_root_slots.is_empty());
        } else {
            // The first vote should become root after 31 confirmations
            assert_eq!(result.new_root_slots, vec![1]);
        }
    }

    assert_eq!(tower.root_slot(), Some(1));

    // Continue voting - root should advance with each vote
    for slot in (MAX_LOCKOUT_HISTORY + 1)..=(MAX_LOCKOUT_HISTORY + 5) {
        let result = tower.process_vote(&make_vote(slot)).unwrap();
        assert!(!result.new_root_slots.is_empty());
    }
}

#[test]
fn test_tower_fork_lockout_enforcement() {
    let mut tower = make_tower();

    // Build tower on fork A: vote on slots 1, 2, 3, 4, 5
    for slot in 1..=5 {
        tower.process_vote(&make_vote(slot)).unwrap();
    }

    // Try to vote on slot 3 (going back) - should fail due to lockout
    let result = tower.process_vote(&make_vote(3));
    assert!(result.is_err());
}

#[test]
fn test_tower_switch_threshold() {
    let tower = make_tower();

    // 38% threshold - should pass with >= 38%
    let stakes_above = vec![(hash(b"v1"), 40)];
    assert!(tower.check_switch_threshold(10, &stakes_above, 100));

    // Should fail with < 38%
    let stakes_below = vec![(hash(b"v1"), 37)];
    assert!(!tower.check_switch_threshold(10, &stakes_below, 100));

    // Exactly 38% - should pass
    let stakes_exact = vec![(hash(b"v1"), 38)];
    assert!(tower.check_switch_threshold(10, &stakes_exact, 100));
}

#[test]
fn test_tower_vote_state_persistence() {
    let mut tower = make_tower();

    // Process several votes
    for slot in 1..=10 {
        tower.process_vote(&make_vote(slot)).unwrap();
    }

    // Serialize and deserialize vote state
    let vote_state = tower.vote_state().clone();
    let encoded = borsh::to_vec(&vote_state).unwrap();
    let decoded: VoteState = borsh::from_slice(&encoded).unwrap();

    // Create new tower from deserialized state
    let mut tower2 = Tower::new(decoded);

    // Should be able to continue processing votes
    let result = tower2.process_vote(&make_vote(11)).unwrap();
    assert!(result.new_root_slots.is_empty());
    assert_eq!(tower2.depth(), 11);
}

#[test]
fn test_tower_expired_lockouts() {
    let mut tower = make_tower();

    // Build a few lockouts
    tower.process_vote(&make_vote(1)).unwrap();
    tower.process_vote(&make_vote(2)).unwrap();
    tower.process_vote(&make_vote(3)).unwrap();

    // Vote at a far future slot - old lockouts should expire
    let result = tower.process_vote(&make_vote(1000)).unwrap();

    // All previous lockouts should have expired since their lockout periods
    // (2^confirmation_count) are much smaller than the gap
    assert!(!result.expired_lockouts.is_empty());
}

#[test]
fn test_tower_full_cycle() {
    let mut tower = make_tower();

    // Build tower to full depth and beyond
    let total_votes = MAX_LOCKOUT_HISTORY * 2;
    let mut roots_found = 0;

    for slot in 1..=total_votes {
        let result = tower.process_vote(&make_vote(slot)).unwrap();
        if !result.new_root_slots.is_empty() {
            roots_found += 1;
        }
    }

    // We should have advanced the root multiple times
    assert!(roots_found > 0);
    assert!(tower.root_slot().is_some());
    assert!(tower.root_slot().unwrap() > 1);
}

#[test]
fn test_tower_lockout_doubling() {
    let mut tower = make_tower();

    // After voting on slots 1-5 sequentially:
    // slot 1 has confirmation_count = 5 (lockout = 2^5 = 32)
    // slot 5 has confirmation_count = 1 (lockout = 2^1 = 2)
    for slot in 1..=5 {
        tower.process_vote(&make_vote(slot)).unwrap();
    }

    let vote_state = tower.vote_state();
    assert_eq!(vote_state.votes[0].confirmation_count, 5);
    assert_eq!(vote_state.votes[0].lockout(), 32);
    assert_eq!(vote_state.votes[4].confirmation_count, 1);
    assert_eq!(vote_state.votes[4].lockout(), 2);
}
