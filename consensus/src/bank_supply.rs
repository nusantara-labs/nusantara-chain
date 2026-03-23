use tracing::instrument;

use crate::bank::ConsensusBank;

impl ConsensusBank {
    /// Get the total active stake.
    pub fn total_active_stake(&self) -> u64 {
        self.epoch_stake_state.read().total_active_stake
    }

    /// Get the total token supply.
    pub fn total_supply(&self) -> u64 {
        *self.total_supply.read()
    }

    /// Set the total supply (initialized from genesis sum of all accounts).
    #[instrument(skip(self), level = "debug")]
    pub fn set_total_supply(&self, supply: u64) {
        *self.total_supply.write() = supply;
        metrics::gauge!("nusantara_bank_total_supply").set(supply as f64);
    }

    /// Deduct burned fees from total supply.
    #[instrument(skip(self), level = "debug")]
    pub fn burn_fees(&self, amount: u64) {
        let mut supply = self.total_supply.write();
        *supply = supply.saturating_sub(amount);
    }
}
