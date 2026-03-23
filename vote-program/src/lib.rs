use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_core::instruction::{AccountMeta, Instruction};
use nusantara_core::native_token::const_parse_u64;
use nusantara_core::program::VOTE_PROGRAM_ID;
use nusantara_crypto::Hash;

pub const MAX_LOCKOUT_HISTORY: u64 = const_parse_u64(env!("NUSA_VOTE_MAX_LOCKOUT_HISTORY"));
pub const MAX_EPOCH_CREDITS_HISTORY: u64 =
    const_parse_u64(env!("NUSA_VOTE_MAX_EPOCH_CREDITS_HISTORY"));

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct VoteState {
    pub node_pubkey: Hash,
    pub authorized_voter: Hash,
    pub authorized_withdrawer: Hash,
    pub commission: u8,
    pub votes: Vec<Lockout>,
    pub root_slot: Option<u64>,
    pub epoch_credits: Vec<(u64, u64, u64)>,
    pub last_timestamp: BlockTimestamp,
}

impl VoteState {
    pub fn new(init: &VoteInit) -> Self {
        Self {
            node_pubkey: init.node_pubkey,
            authorized_voter: init.authorized_voter,
            authorized_withdrawer: init.authorized_withdrawer,
            commission: init.commission,
            votes: Vec::new(),
            root_slot: None,
            epoch_credits: Vec::new(),
            last_timestamp: BlockTimestamp::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Lockout {
    pub slot: u64,
    pub confirmation_count: u32,
}

impl Lockout {
    pub fn lockout(&self) -> u64 {
        2u64.saturating_pow(self.confirmation_count.min(63))
    }

    pub fn is_locked_out_at_slot(&self, slot: u64) -> bool {
        self.slot.saturating_add(self.lockout()) >= slot
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct BlockTimestamp {
    pub slot: u64,
    pub timestamp: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum VoteInstruction {
    InitializeAccount(VoteInit),
    Vote(Vote),
    Authorize(Hash, VoteAuthorize),
    Withdraw(u64),
    UpdateCommission(u8),
    SwitchVote(Vote, Hash),
    UpdateValidatorIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct VoteInit {
    pub node_pubkey: Hash,
    pub authorized_voter: Hash,
    pub authorized_withdrawer: Hash,
    pub commission: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Vote {
    pub slots: Vec<u64>,
    pub hash: Hash,
    pub timestamp: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum VoteAuthorize {
    Voter,
    Withdrawer,
}

pub fn initialize_account(vote_account: &Hash, init: VoteInit) -> Instruction {
    let data = borsh::to_vec(&VoteInstruction::InitializeAccount(init))
        .expect("serialization cannot fail");

    Instruction {
        program_id: *VOTE_PROGRAM_ID,
        accounts: vec![AccountMeta::new(*vote_account, false)],
        data,
    }
}

pub fn vote(vote_account: &Hash, authorized_voter: &Hash, v: Vote) -> Instruction {
    let data = borsh::to_vec(&VoteInstruction::Vote(v)).expect("serialization cannot fail");

    Instruction {
        program_id: *VOTE_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*vote_account, false),
            AccountMeta::new_readonly(*authorized_voter, true),
        ],
        data,
    }
}

pub fn withdraw(
    vote_account: &Hash,
    authorized_withdrawer: &Hash,
    to: &Hash,
    lamports: u64,
) -> Instruction {
    let data =
        borsh::to_vec(&VoteInstruction::Withdraw(lamports)).expect("serialization cannot fail");

    Instruction {
        program_id: *VOTE_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*vote_account, false),
            AccountMeta::new(*to, false),
            AccountMeta::new_readonly(*authorized_withdrawer, true),
        ],
        data,
    }
}

pub fn authorize(
    vote_account: &Hash,
    authorized: &Hash,
    new_authorized: Hash,
    authorize_type: VoteAuthorize,
) -> Instruction {
    let data = borsh::to_vec(&VoteInstruction::Authorize(new_authorized, authorize_type))
        .expect("serialization cannot fail");

    Instruction {
        program_id: *VOTE_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*vote_account, false),
            AccountMeta::new_readonly(*authorized, true),
        ],
        data,
    }
}

pub fn update_commission(
    vote_account: &Hash,
    authorized_withdrawer: &Hash,
    commission: u8,
) -> Instruction {
    let data = borsh::to_vec(&VoteInstruction::UpdateCommission(commission))
        .expect("serialization cannot fail");

    Instruction {
        program_id: *VOTE_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*vote_account, false),
            AccountMeta::new_readonly(*authorized_withdrawer, true),
        ],
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    #[test]
    fn default_config_values() {
        assert_eq!(MAX_LOCKOUT_HISTORY, 31);
        assert_eq!(MAX_EPOCH_CREDITS_HISTORY, 64);
    }

    #[test]
    fn vote_state_borsh_roundtrip() {
        let init = VoteInit {
            node_pubkey: hash(b"node"),
            authorized_voter: hash(b"voter"),
            authorized_withdrawer: hash(b"withdrawer"),
            commission: 10,
        };
        let state = VoteState::new(&init);
        let encoded = borsh::to_vec(&state).unwrap();
        let decoded: VoteState = borsh::from_slice(&encoded).unwrap();
        assert_eq!(state, decoded);
    }

    #[test]
    fn vote_instruction_borsh_roundtrip() {
        let instructions: Vec<VoteInstruction> = vec![
            VoteInstruction::InitializeAccount(VoteInit {
                node_pubkey: hash(b"node"),
                authorized_voter: hash(b"voter"),
                authorized_withdrawer: hash(b"withdrawer"),
                commission: 5,
            }),
            VoteInstruction::Vote(Vote {
                slots: vec![100, 101, 102],
                hash: hash(b"blockhash"),
                timestamp: Some(1234567890),
            }),
            VoteInstruction::Authorize(hash(b"new_voter"), VoteAuthorize::Voter),
            VoteInstruction::Withdraw(50000),
            VoteInstruction::UpdateCommission(15),
            VoteInstruction::UpdateValidatorIdentity,
        ];

        for ix in &instructions {
            let encoded = borsh::to_vec(ix).unwrap();
            let decoded: VoteInstruction = borsh::from_slice(&encoded).unwrap();
            assert_eq!(*ix, decoded);
        }
    }

    #[test]
    fn lockout_calculation() {
        let lockout = Lockout {
            slot: 100,
            confirmation_count: 5,
        };
        assert_eq!(lockout.lockout(), 32);
        assert!(lockout.is_locked_out_at_slot(130));
        assert!(!lockout.is_locked_out_at_slot(133));
    }

    #[test]
    fn lockout_overflow_guard() {
        // confirmation_count = 64 would overflow 2^64; clamped to 63 → 2^63
        let lockout_64 = Lockout {
            slot: 0,
            confirmation_count: 64,
        };
        assert_eq!(lockout_64.lockout(), 1u64 << 63);

        let lockout_max = Lockout {
            slot: 0,
            confirmation_count: u32::MAX,
        };
        // Clamped to 63 → 2^63
        assert_eq!(lockout_max.lockout(), 1u64 << 63);

        // Normal case still works
        let lockout_31 = Lockout {
            slot: 0,
            confirmation_count: 31,
        };
        assert_eq!(lockout_31.lockout(), 1u64 << 31);
    }

    #[test]
    fn lockout_no_overflow_near_u64_max() {
        // Slot near u64::MAX with large lockout should not overflow
        let lockout = Lockout {
            slot: u64::MAX - 10,
            confirmation_count: 32,
        };
        // lockout() = 2^32 = 4_294_967_296, slot + lockout would overflow without saturating_add
        // saturating_add clamps to u64::MAX, so is_locked_out_at_slot should return true
        // for any slot <= u64::MAX
        assert!(lockout.is_locked_out_at_slot(u64::MAX));
        assert!(lockout.is_locked_out_at_slot(u64::MAX - 5));
        assert!(lockout.is_locked_out_at_slot(0));

        // Even with max slot and high confirmation, no panic
        let lockout_max_slot = Lockout {
            slot: u64::MAX,
            confirmation_count: 63,
        };
        assert!(lockout_max_slot.is_locked_out_at_slot(u64::MAX));
        assert!(lockout_max_slot.is_locked_out_at_slot(0));
    }

    #[test]
    fn builder_functions() {
        let vote_acc = hash(b"vote");
        let voter = hash(b"voter");

        let v = Vote {
            slots: vec![42],
            hash: hash(b"hash"),
            timestamp: None,
        };
        let ix = vote(&vote_acc, &voter, v.clone());
        assert_eq!(ix.program_id, *VOTE_PROGRAM_ID);
        assert_eq!(ix.accounts.len(), 2);

        let decoded: VoteInstruction = borsh::from_slice(&ix.data).unwrap();
        assert_eq!(decoded, VoteInstruction::Vote(v));
    }
}
