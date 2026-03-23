use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_core::instruction::{AccountMeta, Instruction};
use nusantara_core::native_token::const_parse_u64;
use nusantara_core::program::STAKE_PROGRAM_ID;
use nusantara_crypto::Hash;

pub const DEFAULT_WARMUP_COOLDOWN_RATE_BPS: u64 =
    const_parse_u64(env!("NUSA_STAKE_WARMUP_COOLDOWN_RATE_BPS"));
pub const DEFAULT_MIN_DELEGATION: u64 =
    const_parse_u64(env!("NUSA_STAKE_MIN_DELEGATION"));

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum StakeStateV2 {
    Uninitialized,
    Initialized(Meta),
    Stake(Meta, Stake),
    RewardsPool,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Meta {
    pub rent_exempt_reserve: u64,
    pub authorized: Authorized,
    pub lockup: Lockup,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Authorized {
    pub staker: Hash,
    pub withdrawer: Hash,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Lockup {
    pub unix_timestamp: i64,
    pub epoch: u64,
    pub custodian: Hash,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Stake {
    pub delegation: Delegation,
    pub credits_observed: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Delegation {
    pub voter_pubkey: Hash,
    pub stake: u64,
    pub activation_epoch: u64,
    pub deactivation_epoch: u64,
    pub warmup_cooldown_rate_bps: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum StakeInstruction {
    Initialize(Authorized, Lockup),
    Authorize(Hash, StakeAuthorize),
    DelegateStake,
    Split(u64),
    Withdraw(u64),
    Deactivate,
    Merge,
    SetLockup(LockupArgs),
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum StakeAuthorize {
    Staker,
    Withdrawer,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct LockupArgs {
    pub unix_timestamp: Option<i64>,
    pub epoch: Option<u64>,
    pub custodian: Option<Hash>,
}

pub fn initialize(stake_account: &Hash, authorized: Authorized, lockup: Lockup) -> Instruction {
    let data = borsh::to_vec(&StakeInstruction::Initialize(authorized, lockup))
        .expect("serialization cannot fail");

    Instruction {
        program_id: *STAKE_PROGRAM_ID,
        accounts: vec![AccountMeta::new(*stake_account, false)],
        data,
    }
}

pub fn delegate_stake(
    stake_account: &Hash,
    vote_account: &Hash,
    staker: &Hash,
) -> Instruction {
    let data =
        borsh::to_vec(&StakeInstruction::DelegateStake).expect("serialization cannot fail");

    Instruction {
        program_id: *STAKE_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*stake_account, false),
            AccountMeta::new_readonly(*vote_account, false),
            AccountMeta::new_readonly(*staker, true),
        ],
        data,
    }
}

pub fn split(
    stake_account: &Hash,
    staker: &Hash,
    split_stake_account: &Hash,
    lamports: u64,
) -> Instruction {
    let data =
        borsh::to_vec(&StakeInstruction::Split(lamports)).expect("serialization cannot fail");

    Instruction {
        program_id: *STAKE_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*stake_account, false),
            AccountMeta::new(*split_stake_account, false),
            AccountMeta::new_readonly(*staker, true),
        ],
        data,
    }
}

pub fn withdraw(
    stake_account: &Hash,
    withdrawer: &Hash,
    to: &Hash,
    lamports: u64,
) -> Instruction {
    let data =
        borsh::to_vec(&StakeInstruction::Withdraw(lamports)).expect("serialization cannot fail");

    Instruction {
        program_id: *STAKE_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*stake_account, false),
            AccountMeta::new(*to, false),
            AccountMeta::new_readonly(*withdrawer, true),
        ],
        data,
    }
}

pub fn deactivate(stake_account: &Hash, staker: &Hash) -> Instruction {
    let data =
        borsh::to_vec(&StakeInstruction::Deactivate).expect("serialization cannot fail");

    Instruction {
        program_id: *STAKE_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*stake_account, false),
            AccountMeta::new_readonly(*staker, true),
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
        assert_eq!(DEFAULT_WARMUP_COOLDOWN_RATE_BPS, 2500);
        assert_eq!(DEFAULT_MIN_DELEGATION, 1_000_000_000);
    }

    #[test]
    fn stake_state_borsh_roundtrip() {
        let auth = Authorized {
            staker: hash(b"staker"),
            withdrawer: hash(b"withdrawer"),
        };
        let lockup = Lockup {
            unix_timestamp: 0,
            epoch: 0,
            custodian: Hash::zero(),
        };
        let meta = Meta {
            rent_exempt_reserve: 2282880,
            authorized: auth,
            lockup,
        };
        let delegation = Delegation {
            voter_pubkey: hash(b"voter"),
            stake: 1_000_000_000,
            activation_epoch: 10,
            deactivation_epoch: u64::MAX,
            warmup_cooldown_rate_bps: 2500,
        };
        let stake = Stake {
            delegation,
            credits_observed: 100,
        };
        let state = StakeStateV2::Stake(meta, stake);

        let encoded = borsh::to_vec(&state).unwrap();
        let decoded: StakeStateV2 = borsh::from_slice(&encoded).unwrap();
        assert_eq!(state, decoded);
    }

    #[test]
    fn stake_instruction_borsh_roundtrip() {
        let instructions: Vec<StakeInstruction> = vec![
            StakeInstruction::Initialize(
                Authorized {
                    staker: hash(b"staker"),
                    withdrawer: hash(b"withdrawer"),
                },
                Lockup {
                    unix_timestamp: 0,
                    epoch: 0,
                    custodian: Hash::zero(),
                },
            ),
            StakeInstruction::Authorize(hash(b"new_auth"), StakeAuthorize::Staker),
            StakeInstruction::DelegateStake,
            StakeInstruction::Split(500_000_000),
            StakeInstruction::Withdraw(100_000),
            StakeInstruction::Deactivate,
            StakeInstruction::Merge,
        ];

        for ix in &instructions {
            let encoded = borsh::to_vec(ix).unwrap();
            let decoded: StakeInstruction = borsh::from_slice(&encoded).unwrap();
            assert_eq!(*ix, decoded);
        }
    }

    #[test]
    fn builder_functions() {
        let stake_acc = hash(b"stake");
        let vote_acc = hash(b"vote");
        let staker = hash(b"staker");

        let ix = delegate_stake(&stake_acc, &vote_acc, &staker);
        assert_eq!(ix.program_id, *STAKE_PROGRAM_ID);
        assert_eq!(ix.accounts.len(), 3);

        let decoded: StakeInstruction = borsh::from_slice(&ix.data).unwrap();
        assert_eq!(decoded, StakeInstruction::DelegateStake);
    }
}
