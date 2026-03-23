use std::sync::LazyLock;

use nusantara_crypto::{Hash, hash};

pub static SYSTEM_PROGRAM_ID: LazyLock<Hash> =
    LazyLock::new(|| hash(env!("NUSA_PROGRAMS_SYSTEM").as_bytes()));

pub static RENT_PROGRAM_ID: LazyLock<Hash> =
    LazyLock::new(|| hash(env!("NUSA_PROGRAMS_RENT").as_bytes()));

pub static STAKE_PROGRAM_ID: LazyLock<Hash> =
    LazyLock::new(|| hash(env!("NUSA_PROGRAMS_STAKE").as_bytes()));

pub static VOTE_PROGRAM_ID: LazyLock<Hash> =
    LazyLock::new(|| hash(env!("NUSA_PROGRAMS_VOTE").as_bytes()));

pub static COMPUTE_BUDGET_PROGRAM_ID: LazyLock<Hash> =
    LazyLock::new(|| hash(env!("NUSA_PROGRAMS_COMPUTE_BUDGET").as_bytes()));

pub static SYSVAR_PROGRAM_ID: LazyLock<Hash> =
    LazyLock::new(|| hash(env!("NUSA_PROGRAMS_SYSVAR").as_bytes()));

pub static LOADER_PROGRAM_ID: LazyLock<Hash> =
    LazyLock::new(|| hash(env!("NUSA_PROGRAMS_LOADER").as_bytes()));

pub static TOKEN_PROGRAM_ID: LazyLock<Hash> =
    LazyLock::new(|| hash(env!("NUSA_PROGRAMS_TOKEN").as_bytes()));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_ids_are_deterministic() {
        let system1 = *SYSTEM_PROGRAM_ID;
        let system2 = hash(b"system_program");
        assert_eq!(system1, system2);
    }

    #[test]
    fn program_ids_are_distinct() {
        assert_ne!(*SYSTEM_PROGRAM_ID, *RENT_PROGRAM_ID);
        assert_ne!(*SYSTEM_PROGRAM_ID, *STAKE_PROGRAM_ID);
        assert_ne!(*RENT_PROGRAM_ID, *VOTE_PROGRAM_ID);
    }
}
