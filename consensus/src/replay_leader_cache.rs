use nusantara_crypto::Hash;

use crate::error::ConsensusError;
use crate::leader_schedule::LeaderSchedule;
use crate::replay_stage::ReplayStage;

impl ReplayStage {
    /// Cache a leader schedule for the given epoch.
    /// Evicts schedules older than 2 epochs to bound memory usage.
    pub fn cache_leader_schedule(&mut self, epoch: u64, schedule: LeaderSchedule) {
        self.leader_schedule_cache.insert(epoch, schedule);
        let min_epoch = epoch.saturating_sub(2);
        self.leader_schedule_cache.retain(|&e, _| e >= min_epoch);
    }

    /// Get or compute leader schedule for the given epoch.
    pub fn get_leader_schedule(
        &mut self,
        epoch: u64,
        epoch_seed: &Hash,
    ) -> Result<&LeaderSchedule, ConsensusError> {
        if !self.leader_schedule_cache.contains_key(&epoch) {
            let stakes = self.bank.get_stake_distribution();
            let schedule = self
                .leader_schedule_generator
                .compute_schedule(epoch, &stakes, epoch_seed)?;
            self.leader_schedule_cache.insert(epoch, schedule);
        }
        Ok(self.leader_schedule_cache.get(&epoch).unwrap())
    }
}
