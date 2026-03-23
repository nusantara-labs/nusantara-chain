//! Sysvar accessors for Nusantara programs.
//!
//! Sysvars provide programs with read-only access to chain state such as the
//! current slot, epoch, and rent parameters. Under WASM these are populated
//! by the VM via `nusa_get_*` syscalls; outside WASM they return default
//! values for testing.

/// Current blockchain timing information.
///
/// Updated by the validator at the beginning of each slot.
#[derive(Debug, Clone, Default)]
pub struct Clock {
    /// Current slot number.
    pub slot: u64,
    /// Current epoch number.
    pub epoch: u64,
    /// Approximate wall-clock Unix timestamp (seconds since epoch).
    pub unix_timestamp: i64,
}

/// Rent parameters that determine the minimum balance for rent exemption.
///
/// Accounts whose balance falls below the rent-exempt minimum are subject to
/// rent collection. Programs should use [`Rent::minimum_balance`] to compute
/// the required balance for a given data size.
#[derive(Debug, Clone)]
pub struct Rent {
    /// Lamports charged per byte per year.
    pub lamports_per_byte_year: u64,
    /// Number of years of rent that makes an account exempt.
    pub exemption_threshold: u64,
    /// Percentage of collected rent that is burned (0-100).
    pub burn_percent: u8,
}

impl Default for Rent {
    fn default() -> Self {
        Self {
            lamports_per_byte_year: 3480,
            exemption_threshold: 2,
            burn_percent: 50,
        }
    }
}

impl Rent {
    /// Compute the minimum lamport balance for rent exemption.
    ///
    /// Includes a 128-byte overhead for account metadata (key, owner, etc.).
    pub fn minimum_balance(&self, data_len: usize) -> u64 {
        let total_size = (data_len + 128) as u64;
        self.lamports_per_byte_year * total_size * self.exemption_threshold
    }
}

/// Epoch schedule configuration.
#[derive(Debug, Clone, Default)]
pub struct EpochSchedule {
    /// Number of slots in each epoch.
    pub slots_per_epoch: u64,
}

/// Read the current `Clock` sysvar from the VM.
///
/// Under `wasm32` this calls `nusa_get_clock`. Outside WASM it returns a
/// default `Clock` (all zeros).
pub fn get_clock() -> Clock {
    #[cfg(target_arch = "wasm32")]
    {
        let mut slot: u64 = 0;
        let mut epoch: u64 = 0;
        let mut timestamp: i64 = 0;
        unsafe {
            crate::syscall::nusa_get_clock(
                &mut slot as *mut u64,
                &mut epoch as *mut u64,
                &mut timestamp as *mut i64,
            );
        }
        Clock {
            slot,
            epoch,
            unix_timestamp: timestamp,
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        Clock::default()
    }
}

/// Read the current `Rent` sysvar from the VM.
///
/// Under `wasm32` this calls `nusa_get_rent`. Outside WASM it returns the
/// default `Rent` values.
pub fn get_rent() -> Rent {
    #[cfg(target_arch = "wasm32")]
    {
        let mut lamports_per_byte_year: u64 = 0;
        let mut exemption_threshold: u64 = 0;
        let mut burn_percent: u8 = 0;
        unsafe {
            crate::syscall::nusa_get_rent(
                &mut lamports_per_byte_year as *mut u64,
                &mut exemption_threshold as *mut u64,
                &mut burn_percent as *mut u8,
            );
        }
        Rent {
            lamports_per_byte_year,
            exemption_threshold,
            burn_percent,
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        Rent::default()
    }
}

/// Read the current `EpochSchedule` sysvar from the VM.
///
/// Under `wasm32` this calls `nusa_get_epoch_schedule`. Outside WASM it
/// returns a default `EpochSchedule`.
pub fn get_epoch_schedule() -> EpochSchedule {
    #[cfg(target_arch = "wasm32")]
    {
        let mut slots_per_epoch: u64 = 0;
        unsafe {
            crate::syscall::nusa_get_epoch_schedule(&mut slots_per_epoch as *mut u64);
        }
        EpochSchedule { slots_per_epoch }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        EpochSchedule::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_default() {
        let clock = Clock::default();
        assert_eq!(clock.slot, 0);
        assert_eq!(clock.epoch, 0);
        assert_eq!(clock.unix_timestamp, 0);
    }

    #[test]
    fn rent_default() {
        let rent = Rent::default();
        assert_eq!(rent.lamports_per_byte_year, 3480);
        assert_eq!(rent.exemption_threshold, 2);
        assert_eq!(rent.burn_percent, 50);
    }

    #[test]
    fn rent_minimum_balance() {
        let rent = Rent::default();
        // 100 data bytes + 128 overhead = 228 total
        // 3480 * 228 * 2 = 1_586_880
        let min = rent.minimum_balance(100);
        assert_eq!(min, 3480 * 228 * 2);
        assert!(min > 0);
    }

    #[test]
    fn rent_minimum_balance_zero_data() {
        let rent = Rent::default();
        // 0 data bytes + 128 overhead = 128 total
        let min = rent.minimum_balance(0);
        assert_eq!(min, 3480 * 128 * 2);
    }

    #[test]
    fn epoch_schedule_default() {
        let schedule = EpochSchedule::default();
        assert_eq!(schedule.slots_per_epoch, 0);
    }

    #[test]
    fn get_clock_returns_default_outside_wasm() {
        let clock = get_clock();
        assert_eq!(clock.slot, 0);
    }

    #[test]
    fn get_rent_returns_default_outside_wasm() {
        let rent = get_rent();
        assert_eq!(rent.lamports_per_byte_year, 3480);
    }

    #[test]
    fn get_epoch_schedule_returns_default_outside_wasm() {
        let schedule = get_epoch_schedule();
        assert_eq!(schedule.slots_per_epoch, 0);
    }
}
