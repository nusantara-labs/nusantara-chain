use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_core::native_token::const_parse_u64;
use nusantara_crypto::{Hash, hash};
use nusantara_stake_program::Delegation;
use nusantara_vote_program::VoteState;
use tracing::instrument;

use nusantara_core::DEFAULT_SLOT_DURATION_MS;

use crate::error::ConsensusError;

pub const PARTITION_COUNT: u64 = const_parse_u64(env!("NUSA_REWARDS_PARTITION_COUNT"));
pub const INITIAL_INFLATION_RATE_BPS: u64 =
    const_parse_u64(env!("NUSA_REWARDS_INITIAL_INFLATION_RATE_BPS"));
pub const TERMINAL_INFLATION_RATE_BPS: u64 =
    const_parse_u64(env!("NUSA_REWARDS_TERMINAL_INFLATION_RATE_BPS"));
pub const TAPER_RATE_BPS: u64 = const_parse_u64(env!("NUSA_REWARDS_TAPER_RATE_BPS"));

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct RewardEntry {
    pub stake_account: Hash,
    pub vote_account: Hash,
    pub lamports: u64,
    pub commission_lamports: u64,
    pub post_balance: u64,
    pub commission: u8,
}

#[derive(Clone, Debug)]
pub struct EpochRewards {
    pub epoch: u64,
    pub total_rewards_lamports: u64,
    pub total_points: u128,
    pub point_value_lamports: u64,
    pub partitions: Vec<Vec<RewardEntry>>,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct RewardDistributionStatus {
    pub epoch: u64,
    pub total_partitions: u32,
    pub distributed_partitions: u32,
    pub total_rewards: u64,
    pub distributed_rewards: u64,
}

pub struct RewardsCalculator;

impl RewardsCalculator {
    /// Calculate the inflation rate for a given epoch in basis points.
    pub fn inflation_rate_bps(epoch: u64) -> u64 {
        // Fast path: after ~100 epochs the rate converges to terminal
        if epoch >= 100 {
            return TERMINAL_INFLATION_RATE_BPS;
        }
        let mut rate = INITIAL_INFLATION_RATE_BPS;
        for _ in 0..epoch {
            rate = rate.saturating_sub(rate * TAPER_RATE_BPS / 10_000);
            if rate <= TERMINAL_INFLATION_RATE_BPS {
                return TERMINAL_INFLATION_RATE_BPS;
            }
        }
        rate
    }

    /// Calculate the total inflation rewards for an epoch given the total supply.
    ///
    /// `slots_per_epoch` determines how many epochs fit in a year. Callers
    /// should pass the actual epoch schedule value (e.g.
    /// `epoch_schedule.slots_per_epoch`) rather than relying on the default.
    pub fn epoch_inflation_rewards(
        epoch: u64,
        total_supply_lamports: u64,
        slots_per_epoch: u64,
    ) -> u64 {
        let rate_bps = Self::inflation_rate_bps(epoch);
        // Annual rate in bps / number of epochs per year
        let ms_per_epoch = slots_per_epoch.saturating_mul(DEFAULT_SLOT_DURATION_MS);
        if ms_per_epoch == 0 {
            return 0;
        }
        let epochs_per_year = 365u64 * 24 * 3600 * 1000 / ms_per_epoch;
        if epochs_per_year == 0 {
            return 0;
        }
        (total_supply_lamports as u128 * rate_bps as u128 / 10_000 / epochs_per_year as u128) as u64
    }

    /// Calculate epoch rewards with partitioned distribution.
    #[instrument(skip(vote_states, delegations), level = "info")]
    pub fn calculate_epoch_rewards(
        epoch: u64,
        inflation_rewards: u64,
        vote_states: &[(Hash, VoteState)],
        delegations: &[(Hash, Delegation)],
    ) -> Result<EpochRewards, ConsensusError> {
        if delegations.is_empty() {
            return Err(ConsensusError::ZeroTotalStake);
        }

        // Calculate total points: credits * stake for each delegation
        let mut total_points: u128 = 0;
        let mut delegation_points: Vec<(usize, u128)> = Vec::with_capacity(delegations.len());

        for (i, (_, delegation)) in delegations.iter().enumerate() {
            let credits = Self::get_epoch_credits(epoch, &delegation.voter_pubkey, vote_states);
            let points = credits as u128 * delegation.stake as u128;
            delegation_points.push((i, points));
            total_points += points;
        }

        if total_points == 0 {
            return Err(ConsensusError::NoEpochCredits);
        }

        // Use u128 throughout to minimize precision loss from integer truncation.
        // Keep a scale factor of 1_000_000 for sub-lamport precision.
        let point_value_scaled = inflation_rewards as u128 * 1_000_000 / total_points;

        // Calculate individual rewards and partition them
        let mut partitions: Vec<Vec<RewardEntry>> =
            (0..PARTITION_COUNT).map(|_| Vec::new()).collect();
        let mut total_distributed: u64 = 0;

        for &(idx, points) in &delegation_points {
            if points == 0 {
                continue;
            }

            let (stake_account, delegation) = &delegations[idx];
            // Use u128 for the full calculation to avoid intermediate truncation
            let reward_lamports = (points * point_value_scaled / 1_000_000) as u64;

            if reward_lamports == 0 {
                continue;
            }

            // Find commission for this voter
            let commission = vote_states
                .iter()
                .find(|(addr, _)| *addr == delegation.voter_pubkey)
                .map(|(_, vs)| vs.commission)
                .unwrap_or(0);

            // Use u128 intermediate for commission split to avoid overflow
            let validator_share = (reward_lamports as u128 * commission as u128 / 100) as u64;
            let staker_share = reward_lamports.saturating_sub(validator_share);

            // Partition by hash(stake_account) % PARTITION_COUNT
            let partition_idx = partition_index(stake_account);

            partitions[partition_idx as usize].push(RewardEntry {
                stake_account: *stake_account,
                vote_account: delegation.voter_pubkey,
                lamports: staker_share,
                commission_lamports: validator_share,
                post_balance: delegation.stake.saturating_add(staker_share),
                commission,
            });

            total_distributed += staker_share + validator_share;
        }

        // Distribute remainder from truncation to the first non-empty partition entry.
        // This ensures total_distributed == inflation_rewards when possible.
        let remainder = inflation_rewards.saturating_sub(total_distributed);
        if remainder > 0
            && let Some(first_entry) = partitions.iter_mut().flat_map(|p| p.iter_mut()).next()
        {
            first_entry.lamports = first_entry.lamports.saturating_add(remainder);
            first_entry.post_balance = first_entry.post_balance.saturating_add(remainder);
            total_distributed = total_distributed.saturating_add(remainder);
        }

        let point_value_lamports = point_value_scaled as u64;

        metrics::counter!("nusantara_rewards_epochs_calculated_total").increment(1);
        metrics::gauge!("nusantara_rewards_total_distributed").set(total_distributed as f64);

        Ok(EpochRewards {
            epoch,
            total_rewards_lamports: total_distributed,
            total_points,
            point_value_lamports,
            partitions,
        })
    }

    fn get_epoch_credits(
        epoch: u64,
        voter_pubkey: &Hash,
        vote_states: &[(Hash, VoteState)],
    ) -> u64 {
        vote_states
            .iter()
            .find(|(addr, _)| addr == voter_pubkey)
            .and_then(|(_, vs)| {
                vs.epoch_credits
                    .iter()
                    .find(|(e, _, _)| *e == epoch)
                    .map(|(_, credits, prev_credits)| credits.saturating_sub(*prev_credits))
            })
            .unwrap_or(0)
    }
}

fn partition_index(account: &Hash) -> u64 {
    let h = hash(account.as_bytes());
    let bytes = h.as_bytes();
    let val = u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]);
    val % PARTITION_COUNT
}

impl RewardDistributionStatus {
    pub fn new(epoch: u64, rewards: &EpochRewards) -> Self {
        Self {
            epoch,
            total_partitions: rewards.partitions.len() as u32,
            distributed_partitions: 0,
            total_rewards: rewards.total_rewards_lamports,
            distributed_rewards: 0,
        }
    }

    pub fn record_partition_distributed(&mut self, partition_rewards: u64) {
        self.distributed_partitions = self.distributed_partitions.saturating_add(1);
        self.distributed_rewards = self.distributed_rewards.saturating_add(partition_rewards);
    }

    pub fn is_complete(&self) -> bool {
        self.distributed_partitions >= self.total_partitions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_vote_program::VoteInit;

    fn make_vote_state(
        node: Hash,
        commission: u8,
        epoch_credits: Vec<(u64, u64, u64)>,
    ) -> VoteState {
        let mut vs = VoteState::new(&VoteInit {
            node_pubkey: node,
            authorized_voter: node,
            authorized_withdrawer: node,
            commission,
        });
        vs.epoch_credits = epoch_credits;
        vs
    }

    #[test]
    fn config_values() {
        assert_eq!(PARTITION_COUNT, 4096);
        assert_eq!(INITIAL_INFLATION_RATE_BPS, 800);
        assert_eq!(TERMINAL_INFLATION_RATE_BPS, 150);
        assert_eq!(TAPER_RATE_BPS, 1500);
    }

    #[test]
    fn inflation_rate_tapers() {
        let r0 = RewardsCalculator::inflation_rate_bps(0);
        let r1 = RewardsCalculator::inflation_rate_bps(1);
        let r100 = RewardsCalculator::inflation_rate_bps(100);

        assert_eq!(r0, INITIAL_INFLATION_RATE_BPS);
        assert!(r1 < r0);
        assert_eq!(r100, TERMINAL_INFLATION_RATE_BPS);
    }

    #[test]
    fn calculate_rewards_basic() {
        let voter = nusantara_crypto::hash(b"voter1");
        let vote_states = vec![(voter, make_vote_state(voter, 10, vec![(1, 100, 0)]))];

        let stake_acc = nusantara_crypto::hash(b"staker1");
        let delegations = vec![(
            stake_acc,
            Delegation {
                voter_pubkey: voter,
                stake: 1_000_000_000,
                activation_epoch: 0,
                deactivation_epoch: u64::MAX,
                warmup_cooldown_rate_bps: 2500,
            },
        )];

        let rewards =
            RewardsCalculator::calculate_epoch_rewards(1, 1_000_000, &vote_states, &delegations)
                .unwrap();

        assert_eq!(rewards.epoch, 1);
        assert!(rewards.total_rewards_lamports > 0);
        assert!(rewards.total_points > 0);

        // Check partitions contain the reward (staker + commission)
        let total_in_partitions: u64 = rewards
            .partitions
            .iter()
            .flat_map(|p| p.iter())
            .map(|e| e.lamports + e.commission_lamports)
            .sum();
        assert_eq!(total_in_partitions, rewards.total_rewards_lamports);
    }

    #[test]
    fn calculate_rewards_empty_delegations() {
        let result = RewardsCalculator::calculate_epoch_rewards(1, 1_000_000, &[], &[]);
        assert!(result.is_err());
    }

    #[test]
    fn distribution_status_tracking() {
        let voter = nusantara_crypto::hash(b"voter");
        let vote_states = vec![(voter, make_vote_state(voter, 5, vec![(1, 50, 0)]))];
        let stake_acc = nusantara_crypto::hash(b"staker");
        let delegations = vec![(
            stake_acc,
            Delegation {
                voter_pubkey: voter,
                stake: 1_000_000_000,
                activation_epoch: 0,
                deactivation_epoch: u64::MAX,
                warmup_cooldown_rate_bps: 2500,
            },
        )];

        let rewards =
            RewardsCalculator::calculate_epoch_rewards(1, 1_000_000, &vote_states, &delegations)
                .unwrap();

        let mut status = RewardDistributionStatus::new(1, &rewards);
        assert!(!status.is_complete());

        for partition in &rewards.partitions {
            let partition_total: u64 = partition.iter().map(|e| e.lamports).sum();
            status.record_partition_distributed(partition_total);
        }
        assert!(status.is_complete());
    }

    #[test]
    fn partition_distribution() {
        // Verify rewards spread across partitions
        let voter = nusantara_crypto::hash(b"voter");
        let vote_states = vec![(voter, make_vote_state(voter, 0, vec![(1, 1000, 0)]))];

        let delegations: Vec<(Hash, Delegation)> = (0u64..100)
            .map(|i| {
                let acc = nusantara_crypto::hashv(&[b"staker", &i.to_le_bytes()]);
                (
                    acc,
                    Delegation {
                        voter_pubkey: voter,
                        stake: 1_000_000_000,
                        activation_epoch: 0,
                        deactivation_epoch: u64::MAX,
                        warmup_cooldown_rate_bps: 2500,
                    },
                )
            })
            .collect();

        let rewards =
            RewardsCalculator::calculate_epoch_rewards(1, 100_000_000, &vote_states, &delegations)
                .unwrap();

        let non_empty: usize = rewards.partitions.iter().filter(|p| !p.is_empty()).count();
        // With 100 stakers and 4096 partitions, most will be empty but some should be populated
        assert!(non_empty > 0);
        assert!(non_empty <= 100);
    }

    #[test]
    fn epoch_inflation_rewards_varies_with_slots_per_epoch() {
        let total_supply = 1_000_000_000_000_000u64; // 1M NUSA
        let epoch = 1;

        // Default (432,000 slots/epoch)
        let default_rewards =
            RewardsCalculator::epoch_inflation_rewards(epoch, total_supply, 432_000);
        assert!(default_rewards > 0, "default rewards should be positive");

        // Fewer slots per epoch -> more epochs per year -> smaller per-epoch reward
        let short_epoch_rewards =
            RewardsCalculator::epoch_inflation_rewards(epoch, total_supply, 100_000);
        assert!(
            short_epoch_rewards < default_rewards,
            "shorter epochs (more per year) should yield smaller per-epoch rewards: {} < {}",
            short_epoch_rewards,
            default_rewards
        );

        // More slots per epoch -> fewer epochs per year -> larger per-epoch reward
        let long_epoch_rewards =
            RewardsCalculator::epoch_inflation_rewards(epoch, total_supply, 1_000_000);
        assert!(
            long_epoch_rewards > default_rewards,
            "longer epochs (fewer per year) should yield larger per-epoch rewards: {} > {}",
            long_epoch_rewards,
            default_rewards
        );

        // Zero slots_per_epoch should return 0 (avoid division by zero)
        let zero_rewards = RewardsCalculator::epoch_inflation_rewards(epoch, total_supply, 0);
        assert_eq!(zero_rewards, 0, "zero slots_per_epoch should yield zero");
    }

    #[test]
    fn total_distributed_equals_inflation_rewards() {
        // Verify that remainder distribution ensures no precision loss.
        // Use large inflation_rewards relative to total_points to ensure
        // per-delegation rewards are non-zero.
        let voter = nusantara_crypto::hash(b"voter");
        let vote_states = vec![(voter, make_vote_state(voter, 10, vec![(1, 1000, 0)]))];

        // 7 delegations * 1000 credits * 1_000 stake = 7_000_000 total_points
        // inflation_rewards = 10_000_003 -> point_value_scaled > 0
        let inflation_rewards = 10_000_003u64;

        let delegations: Vec<(Hash, Delegation)> = (0u64..7)
            .map(|i| {
                let acc = nusantara_crypto::hashv(&[b"staker", &i.to_le_bytes()]);
                (
                    acc,
                    Delegation {
                        voter_pubkey: voter,
                        stake: 1_000,
                        activation_epoch: 0,
                        deactivation_epoch: u64::MAX,
                        warmup_cooldown_rate_bps: 2500,
                    },
                )
            })
            .collect();

        let rewards = RewardsCalculator::calculate_epoch_rewards(
            1,
            inflation_rewards,
            &vote_states,
            &delegations,
        )
        .unwrap();

        // Total distributed should equal inflation_rewards (remainder distributed)
        assert_eq!(
            rewards.total_rewards_lamports, inflation_rewards,
            "total_rewards_lamports ({}) should equal inflation_rewards ({})",
            rewards.total_rewards_lamports, inflation_rewards
        );

        // Cross-check: sum from partitions should match
        let sum_from_partitions: u64 = rewards
            .partitions
            .iter()
            .flat_map(|p| p.iter())
            .map(|e| e.lamports + e.commission_lamports)
            .sum();
        assert_eq!(sum_from_partitions, inflation_rewards);
    }
}
