use std::sync::Arc;

use nusantara_consensus::bank::ConsensusBank;
use nusantara_core::{Account, EpochSchedule};
use nusantara_crypto::Hash;
use nusantara_rent_program::{Rent, RentDue};
use nusantara_storage::{Storage, StorageWriteBatch};
use tracing::{info, warn};

use crate::constants::{MS_PER_YEAR, RENT_PARTITIONS};
use crate::helpers;
use crate::node::ValidatorNode;

impl ValidatorNode {
    pub(crate) async fn check_epoch_boundary(&mut self, snapshot_interval: u64) {
        let current_epoch = self.epoch_schedule.get_epoch(self.current_slot);
        let next_epoch = self.epoch_schedule.get_epoch(self.current_slot + 1);

        if next_epoch > current_epoch {
            // 0 + 1. Collect rent and distribute rewards concurrently — both are
            // independent blocking I/O operations that read from storage but write
            // to separate account sets. Running them in parallel via tokio::join!
            // on two spawn_blocking tasks halves the wall-clock time at epoch boundaries.
            let (rent_deltas, reward_deltas) = {
                let storage_rent = Arc::clone(&self.storage);
                let bank_rent = Arc::clone(&self.bank);
                let rent = self.rent.clone();
                let epoch_schedule = self.epoch_schedule.clone();
                let current_slot = self.current_slot;
                let epoch = current_epoch;

                let storage_reward = Arc::clone(&self.storage);
                let bank_reward = Arc::clone(&self.bank);

                tokio::join!(
                    tokio::task::spawn_blocking(move || {
                        collect_rent_blocking(
                            &storage_rent,
                            &bank_rent,
                            &rent,
                            &epoch_schedule,
                            epoch,
                            current_slot,
                        )
                    }),
                    tokio::task::spawn_blocking(move || {
                        distribute_epoch_rewards_blocking(
                            &storage_reward,
                            &bank_reward,
                            epoch,
                            current_slot,
                        )
                    })
                )
            };
            let rent_deltas = rent_deltas.unwrap_or_else(|e| {
                warn!(error = %e, "rent collection task panicked");
                Vec::new()
            });
            let reward_deltas = reward_deltas.unwrap_or_else(|e| {
                warn!(error = %e, "reward distribution task panicked");
                Vec::new()
            });
            if !rent_deltas.is_empty() {
                self.bank.update_state_tree(&rent_deltas);
            }
            if !reward_deltas.is_empty() {
                self.bank.update_state_tree(&reward_deltas);
            }

            // 2. Stake transitions: the bank already marks delegations active at
            // activation_epoch via set_stake_delegation. Cooldown removal is
            // handled here: fully cooled-down delegations (deactivation_epoch + warmup
            // period fully elapsed) are evicted from the in-memory bank.
            {
                let delegations = self.bank.get_all_delegations();
                let rate_bps = nusantara_stake_program::DEFAULT_WARMUP_COOLDOWN_RATE_BPS;
                for (stake_account, delegation) in &delegations {
                    if delegation.deactivation_epoch != u64::MAX {
                        let epochs_deactivating =
                            next_epoch.saturating_sub(delegation.deactivation_epoch);
                        let cooldown_bps = epochs_deactivating.saturating_mul(rate_bps);
                        if cooldown_bps >= 10_000 {
                            self.bank.remove_stake_delegation(stake_account);
                        }
                    }
                }
            }

            // 3. Update stake history sysvar
            let total_stake = self.bank.total_active_stake();
            self.bank.update_stake_history(
                current_epoch,
                nusantara_sysvar_program::StakeHistoryEntry {
                    effective: total_stake,
                    activating: 0,
                    deactivating: 0,
                },
            );

            // 4. Recalculate epoch stakes for next epoch
            self.bank.recalculate_epoch_stakes(next_epoch);

            // 5. Compute leader schedule for next epoch
            let stakes = self.bank.get_stake_distribution();
            if let Ok(schedule) = self.leader_schedule_generator.compute_schedule(
                next_epoch,
                &stakes,
                &self.genesis_hash,
            ) {
                self.replay_stage
                    .cache_leader_schedule(next_epoch, schedule.clone());
                self.leader_cache.lock().put(next_epoch, schedule);
            }

            info!(
                epoch = next_epoch,
                total_stake = self.bank.total_active_stake(),
                "epoch boundary crossed"
            );

            // 6. Create snapshot at epoch boundary if configured
            if snapshot_interval > 0 && next_epoch.is_multiple_of(snapshot_interval) {
                self.create_snapshot();
            }
        }
    }

    fn create_snapshot(&self) {
        use nusantara_storage::snapshot_archive;

        let bank_hash = self
            .bank
            .slot_hashes()
            .0
            .first()
            .map(|(_, h)| *h)
            .unwrap_or(Hash::zero());

        let timestamp = helpers::unix_timestamp_secs();
        let storage = Arc::clone(&self.storage);
        let current_slot = self.current_slot;
        let snapshot_dir = self.snapshot_dir.clone();

        // Snapshot creation reads from RocksDB (blocking I/O) — offload to
        // a blocking thread to avoid stalling the async slot loop.
        tokio::task::spawn_blocking(move || {
            match snapshot_archive::create_snapshot(&storage, current_slot, bank_hash, timestamp) {
                Ok(archive) => {
                    if let Err(e) = std::fs::create_dir_all(&snapshot_dir) {
                        tracing::error!(
                            error = %e,
                            dir = %snapshot_dir.display(),
                            "failed to create snapshot directory"
                        );
                        metrics::counter!("nusantara_snapshot_creation_failed").increment(1);
                        return;
                    }
                    let path = snapshot_dir.join(format!("snapshot-{current_slot}.bin"));
                    if let Err(e) = snapshot_archive::save_to_file(&archive, &path) {
                        tracing::error!(
                            error = %e,
                            path = %path.display(),
                            "failed to save snapshot file"
                        );
                        metrics::counter!("nusantara_snapshot_creation_failed").increment(1);
                    } else {
                        tracing::info!(
                            slot = current_slot,
                            accounts = archive.manifest.account_count,
                            path = %path.display(),
                            "snapshot created"
                        );
                        metrics::counter!("nusantara_snapshot_creation_succeeded").increment(1);
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, slot = current_slot, "failed to create snapshot");
                    metrics::counter!("nusantara_snapshot_creation_failed").increment(1);
                }
            }
        });
    }

}

/// Freestanding rent collection to run in a blocking thread.
///
/// All rent-adjusted account writes are accumulated into a single
/// `StorageWriteBatch` and committed atomically. On commit failure the
/// fees are NOT burned, preventing an inconsistency between burned fees
/// and the actual on-disk account balances.
///
/// Returns account deltas so the caller can update the state Merkle tree.
fn collect_rent_blocking(
    storage: &Storage,
    bank: &ConsensusBank,
    rent: &Rent,
    epoch_schedule: &EpochSchedule,
    epoch: u64,
    current_slot: u64,
) -> Vec<(Hash, Account)> {
    let partition = epoch % RENT_PARTITIONS;
    let mut rent_collected = 0u64;
    let mut accounts_closed = 0u64;
    let mut deltas = Vec::new();

    let ms_per_epoch = epoch_schedule.slots_per_epoch * nusantara_core::DEFAULT_SLOT_DURATION_MS;

    let mut batch = StorageWriteBatch::new();

    if let Ok(accounts) = storage.get_accounts_in_partition(partition, RENT_PARTITIONS) {
        for (address, mut account) in accounts {
            let rent_due = match rent.due_epoch(
                account.lamports,
                account.data.len(),
                ms_per_epoch,
                MS_PER_YEAR,
            ) {
                RentDue::Exempt => continue,
                RentDue::Paying(amount) => amount,
            };

            if rent_due == 0 {
                continue;
            }

            let old_account = account.clone();

            if account.lamports <= rent_due {
                rent_collected += account.lamports;
                account.lamports = 0;
                account.data.clear();
                accounts_closed += 1;
            } else {
                account.lamports -= rent_due;
                rent_collected += rent_due;
            }

            if let Err(e) = Storage::append_account_write_with_old(
                &mut batch,
                &address,
                current_slot,
                &account,
                Some(&old_account),
            ) {
                warn!(
                    error = %e,
                    address = %address.to_base64(),
                    "failed to serialize account for rent collection — skipping"
                );
                continue;
            }
            deltas.push((address, account));
        }
    }

    if rent_collected > 0 {
        match storage.write(&batch) {
            Ok(()) => {
                bank.burn_fees(rent_collected);
                info!(
                    epoch,
                    partition, rent_collected, accounts_closed, "rent collected"
                );
            }
            Err(e) => {
                warn!(
                    error = %e,
                    epoch,
                    partition,
                    "atomic rent collection commit failed — fees NOT burned"
                );
                deltas.clear();
            }
        }
    }

    deltas
}

/// Freestanding reward distribution to run in a blocking thread.
///
/// All reward account writes are accumulated into a single `StorageWriteBatch`
/// and committed atomically. Bank state (delegation stakes, total supply) is
/// only updated after the storage commit succeeds.
///
/// Returns account deltas so the caller can update the state Merkle tree.
fn distribute_epoch_rewards_blocking(
    storage: &Storage,
    bank: &ConsensusBank,
    epoch: u64,
    current_slot: u64,
) -> Vec<(Hash, Account)> {
    use nusantara_consensus::rewards::RewardsCalculator;

    let vote_states = bank.get_all_vote_states();
    let delegations = bank.get_all_delegations();

    if delegations.is_empty() {
        return Vec::new();
    }

    let total_supply = bank.total_supply();
    let slots_per_epoch = bank.epoch_schedule().slots_per_epoch;
    let inflation_rewards =
        RewardsCalculator::epoch_inflation_rewards(epoch, total_supply, slots_per_epoch);

    match RewardsCalculator::calculate_epoch_rewards(
        epoch,
        inflation_rewards,
        &vote_states,
        &delegations,
    ) {
        Ok(rewards) => {
            let mut batch = StorageWriteBatch::new();
            let mut total_distributed = 0u64;
            let mut delegation_updates: Vec<(Hash, u64)> = Vec::new();
            let mut deltas: Vec<(Hash, Account)> = Vec::new();

            for partition in rewards.partitions.values() {
                for entry in partition {
                    if let Ok(Some(mut account)) = storage.get_account(&entry.stake_account) {
                        let old_account = account.clone();
                        account.lamports = account.lamports.saturating_add(entry.lamports);
                        if let Err(e) = Storage::append_account_write_with_old(
                            &mut batch,
                            &entry.stake_account,
                            current_slot,
                            &account,
                            Some(&old_account),
                        ) {
                            warn!(
                                error = %e,
                                account = %entry.stake_account.to_base64(),
                                "failed to serialize stake account for rewards — skipping"
                            );
                            continue;
                        }
                        delegation_updates.push((entry.stake_account, account.lamports));
                        deltas.push((entry.stake_account, account.clone()));
                        total_distributed += entry.lamports;
                    }

                    if entry.commission_lamports > 0
                        && let Ok(Some(mut vote_account)) = storage.get_account(&entry.vote_account)
                    {
                        let old_vote = vote_account.clone();
                        vote_account.lamports = vote_account
                            .lamports
                            .saturating_add(entry.commission_lamports);
                        if let Err(e) = Storage::append_account_write_with_old(
                            &mut batch,
                            &entry.vote_account,
                            current_slot,
                            &vote_account,
                            Some(&old_vote),
                        ) {
                            warn!(
                                error = %e,
                                account = %entry.vote_account.to_base64(),
                                "failed to serialize vote account for commission — skipping"
                            );
                            continue;
                        }
                        deltas.push((entry.vote_account, vote_account));
                        total_distributed += entry.commission_lamports;
                    }
                }
            }

            match storage.write(&batch) {
                Ok(()) => {
                    for (stake_account, lamports) in delegation_updates {
                        bank.update_delegation_stake(&stake_account, lamports);
                    }
                    bank.set_total_supply(total_supply.saturating_add(total_distributed));
                    info!(
                        epoch,
                        total_rewards = total_distributed,
                        "epoch rewards distributed"
                    );
                    deltas
                }
                Err(e) => {
                    warn!(
                        epoch,
                        error = %e,
                        "atomic epoch reward commit failed — rewards NOT distributed"
                    );
                    Vec::new()
                }
            }
        }
        Err(e) => {
            warn!(epoch, error = %e, "failed to calculate epoch rewards");
            Vec::new()
        }
    }
}
