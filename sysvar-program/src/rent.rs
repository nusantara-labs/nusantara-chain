use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::{Hash, hash};
use nusantara_rent_program::Rent;

use crate::Sysvar;

#[derive(Clone, Debug, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct RentSysvar(pub Rent);

impl Sysvar for RentSysvar {
    fn id() -> Hash {
        hash(b"sysvar_rent")
    }

    fn size_of() -> usize {
        // Rent: u64 + u64 + u8 = 17 bytes
        17
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borsh_roundtrip() {
        let sysvar = RentSysvar::default();
        let encoded = borsh::to_vec(&sysvar).unwrap();
        let decoded: RentSysvar = borsh::from_slice(&encoded).unwrap();
        assert_eq!(sysvar, decoded);
    }
}
