//! Sysvar access types for WASM programs.
//!
//! These structs define the data layout that WASM programs receive when they
//! request sysvar data. The executor serializes the active sysvar values into
//! these structs and writes them into WASM linear memory.
//!
//! The actual sysvar state is held by the runtime's `SysvarCache` and passed
//! to the VM via the host state. These types serve as the ABI contract between
//! the VM and on-chain programs.
//!
//! ## Serialization
//!
//! All types derive `BorshSerialize` / `BorshDeserialize` for deterministic
//! binary encoding when writing to WASM linear memory or reading from it.
//! Note that `nusantara-sysvar-program` defines richer sysvar types with the
//! same Borsh encoding -- these structs are intentionally leaner ABI views.

use borsh::{BorshDeserialize, BorshSerialize};

/// Clock sysvar: current slot, epoch, and wall-clock time.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ClockInfo {
    pub slot: u64,
    pub epoch: u64,
    pub unix_timestamp: i64,
}

/// Rent sysvar: rent parameters.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct RentInfo {
    pub lamports_per_byte_year: u64,
    pub exemption_threshold: u64,
    pub burn_percent: u8,
}

/// Epoch schedule sysvar: epoch sizing.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct EpochScheduleInfo {
    pub slots_per_epoch: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use borsh::BorshDeserialize;

    #[test]
    fn clock_info_construction() {
        let info = ClockInfo {
            slot: 42,
            epoch: 1,
            unix_timestamp: 1_700_000_000,
        };
        assert_eq!(info.slot, 42);
        assert_eq!(info.epoch, 1);
        assert_eq!(info.unix_timestamp, 1_700_000_000);
    }

    #[test]
    fn rent_info_construction() {
        let info = RentInfo {
            lamports_per_byte_year: 3480,
            exemption_threshold: 2,
            burn_percent: 50,
        };
        assert_eq!(info.lamports_per_byte_year, 3480);
        assert_eq!(info.exemption_threshold, 2);
        assert_eq!(info.burn_percent, 50);
    }

    #[test]
    fn epoch_schedule_construction() {
        let info = EpochScheduleInfo {
            slots_per_epoch: 432_000,
        };
        assert_eq!(info.slots_per_epoch, 432_000);
    }

    #[test]
    fn clock_info_borsh_round_trip() {
        let original = ClockInfo {
            slot: 99,
            epoch: 3,
            unix_timestamp: -1,
        };
        let bytes = borsh::to_vec(&original).unwrap();
        let decoded = ClockInfo::try_from_slice(&bytes).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn rent_info_borsh_round_trip() {
        let original = RentInfo {
            lamports_per_byte_year: 3480,
            exemption_threshold: 2,
            burn_percent: 50,
        };
        let bytes = borsh::to_vec(&original).unwrap();
        let decoded = RentInfo::try_from_slice(&bytes).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn epoch_schedule_borsh_round_trip() {
        let original = EpochScheduleInfo { slots_per_epoch: 432_000 };
        let bytes = borsh::to_vec(&original).unwrap();
        let decoded = EpochScheduleInfo::try_from_slice(&bytes).unwrap();
        assert_eq!(original, decoded);
    }
}
