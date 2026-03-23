use borsh::{BorshDeserialize, BorshSerialize};

use crate::native_token::const_parse_u64;

pub const DEFAULT_LAMPORTS_PER_SIGNATURE: u64 =
    const_parse_u64(env!("NUSA_FEE_LAMPORTS_PER_SIGNATURE"));

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct FeeCalculator {
    pub lamports_per_signature: u64,
}

impl FeeCalculator {
    pub fn new(lamports_per_signature: u64) -> Self {
        Self {
            lamports_per_signature,
        }
    }

    pub fn calculate_fee(&self, num_signatures: u64) -> u64 {
        self.lamports_per_signature.saturating_mul(num_signatures)
    }
}

impl Default for FeeCalculator {
    fn default() -> Self {
        Self::new(DEFAULT_LAMPORTS_PER_SIGNATURE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_fee() {
        assert_eq!(DEFAULT_LAMPORTS_PER_SIGNATURE, 5000);
    }

    #[test]
    fn calculate_fee() {
        let calc = FeeCalculator::default();
        assert_eq!(calc.calculate_fee(1), 5000);
        assert_eq!(calc.calculate_fee(3), 15000);
    }

    #[test]
    fn borsh_roundtrip() {
        let calc = FeeCalculator::new(10000);
        let encoded = borsh::to_vec(&calc).unwrap();
        let decoded: FeeCalculator = borsh::from_slice(&encoded).unwrap();
        assert_eq!(calc, decoded);
    }
}
