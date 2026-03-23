use borsh::{BorshDeserialize, BorshSerialize};

use nusantara_core::native_token::{const_parse_u64, const_parse_u8};

pub const DEFAULT_LAMPORTS_PER_BYTE_YEAR: u64 =
    const_parse_u64(env!("NUSA_RENT_LAMPORTS_PER_BYTE_YEAR"));
pub const DEFAULT_EXEMPTION_THRESHOLD: u64 =
    const_parse_u64(env!("NUSA_RENT_EXEMPTION_THRESHOLD"));
pub const DEFAULT_BURN_PERCENT: u8 = const_parse_u8(env!("NUSA_RENT_BURN_PERCENT"));

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Rent {
    pub lamports_per_byte_year: u64,
    pub exemption_threshold: u64,
    pub burn_percent: u8,
}

impl Rent {
    pub fn minimum_balance(&self, data_len: usize) -> u64 {
        let bytes = 128u64 + data_len as u64; // account overhead + data
        bytes
            .saturating_mul(self.lamports_per_byte_year)
            .saturating_mul(self.exemption_threshold)
    }

    pub fn due(&self, lamports: u64, data_len: usize, years_elapsed: f64) -> RentDue {
        let min = self.minimum_balance(data_len);
        if lamports >= min {
            return RentDue::Exempt;
        }

        let bytes = 128u64 + data_len as u64;
        let rent = (bytes as f64 * self.lamports_per_byte_year as f64 * years_elapsed) as u64;
        RentDue::Paying(rent)
    }

    /// Integer-only rent calculation for consensus-critical paths.
    ///
    /// Uses u128 intermediate arithmetic to avoid floating-point non-determinism
    /// across CPU architectures. `ms_per_epoch` is the epoch duration in
    /// milliseconds, `ms_per_year` is the year duration in milliseconds.
    ///
    /// rent_due = bytes * lamports_per_byte_year * ms_per_epoch / ms_per_year
    pub fn due_epoch(
        &self,
        lamports: u64,
        data_len: usize,
        ms_per_epoch: u64,
        ms_per_year: u64,
    ) -> RentDue {
        let min = self.minimum_balance(data_len);
        if lamports >= min {
            return RentDue::Exempt;
        }

        let bytes = 128u128 + data_len as u128;
        let rent = bytes
            .saturating_mul(self.lamports_per_byte_year as u128)
            .saturating_mul(ms_per_epoch as u128)
            / ms_per_year as u128;
        RentDue::Paying(rent as u64)
    }
}

impl Default for Rent {
    fn default() -> Self {
        Self {
            lamports_per_byte_year: DEFAULT_LAMPORTS_PER_BYTE_YEAR,
            exemption_threshold: DEFAULT_EXEMPTION_THRESHOLD,
            burn_percent: DEFAULT_BURN_PERCENT,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RentDue {
    Exempt,
    Paying(u64),
}

#[derive(Clone, Debug)]
pub struct RentCollector {
    pub epoch: u64,
    pub slots_per_year: f64,
    pub rent: Rent,
}

impl RentCollector {
    pub fn new(epoch: u64, slots_per_year: f64, rent: Rent) -> Self {
        Self {
            epoch,
            slots_per_year,
            rent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rent_values() {
        assert_eq!(DEFAULT_LAMPORTS_PER_BYTE_YEAR, 3480);
        assert_eq!(DEFAULT_EXEMPTION_THRESHOLD, 2);
        assert_eq!(DEFAULT_BURN_PERCENT, 50);
    }

    #[test]
    fn minimum_balance() {
        let rent = Rent::default();
        // 128 (overhead) + 0 (data) = 128 bytes
        // 128 * 3480 * 2 = 890880
        assert_eq!(rent.minimum_balance(0), 890_880);
    }

    #[test]
    fn due_exempt() {
        let rent = Rent::default();
        let min = rent.minimum_balance(100);
        assert_eq!(rent.due(min, 100, 1.0), RentDue::Exempt);
        assert_eq!(rent.due(min + 1, 100, 1.0), RentDue::Exempt);
    }

    #[test]
    fn due_paying() {
        let rent = Rent::default();
        let due = rent.due(0, 100, 1.0);
        assert!(matches!(due, RentDue::Paying(_)));
    }

    #[test]
    fn due_epoch_exempt() {
        let rent = Rent::default();
        let min = rent.minimum_balance(100);
        // 432_000 slots * 400ms = 172_800_000 ms per epoch
        let ms_per_epoch = 432_000u64 * 400;
        let ms_per_year = 31_536_000_000u64;
        assert_eq!(
            rent.due_epoch(min, 100, ms_per_epoch, ms_per_year),
            RentDue::Exempt
        );
    }

    #[test]
    fn due_epoch_paying() {
        let rent = Rent::default();
        let ms_per_epoch = 432_000u64 * 400;
        let ms_per_year = 31_536_000_000u64;
        let due = rent.due_epoch(0, 100, ms_per_epoch, ms_per_year);
        assert!(matches!(due, RentDue::Paying(amount) if amount > 0));
    }

    #[test]
    fn due_epoch_matches_due_approximately() {
        let rent = Rent::default();
        let ms_per_epoch = 432_000u64 * 400;
        let ms_per_year = 31_536_000_000u64;
        let years_per_epoch = ms_per_epoch as f64 / ms_per_year as f64;

        // Compare integer vs float for 100-byte data
        let float_due = rent.due(0, 100, years_per_epoch);
        let int_due = rent.due_epoch(0, 100, ms_per_epoch, ms_per_year);

        if let (RentDue::Paying(f), RentDue::Paying(i)) = (float_due, int_due) {
            // Allow 1 lamport rounding difference
            assert!(f.abs_diff(i) <= 1, "float={f} int={i}");
        } else {
            panic!("both should be Paying");
        }
    }

    #[test]
    fn borsh_roundtrip() {
        let rent = Rent::default();
        let encoded = borsh::to_vec(&rent).unwrap();
        let decoded: Rent = borsh::from_slice(&encoded).unwrap();
        assert_eq!(rent, decoded);
    }
}
