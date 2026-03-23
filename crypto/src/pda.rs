//! Program Derived Address (PDA) derivation.
//!
//! PDAs are deterministic addresses derived from a set of seeds and a program
//! ID. They are used to create accounts that are "owned" by a program without
//! requiring a corresponding private key. This is the fundamental mechanism
//! for programs to sign on behalf of derived accounts during cross-program
//! invocations.
//!
//! ## Derivation scheme
//!
//! ```text
//! PDA = SHA3-512(len(seed_0) ++ seed_0 ++ len(seed_1) ++ seed_1 ++ ... ++ program_id ++ "ProgramDerivedAddress")
//! ```
//!
//! Each seed is length-prefixed (4-byte LE u32) before hashing to prevent
//! concatenation ambiguity (e.g., `["ab","c"]` vs `["a","bc"]`).
//!
//! ## Constraints
//!
//! - Each individual seed must be at most [`MAX_SEED_LEN`] bytes (32).
//! - At most [`MAX_SEEDS`] seeds are allowed per derivation (16).

use crate::error::CryptoError;
use crate::hash::{Hash, Hasher};

/// Maximum length of a single seed in bytes.
pub const MAX_SEED_LEN: usize = 32;

/// Maximum number of seeds allowed in a PDA derivation.
pub const MAX_SEEDS: usize = 16;

/// Create a program-derived address from seeds and a program ID.
///
/// The address is computed as:
/// ```text
/// SHA3-512(seed_0 ++ seed_1 ++ ... ++ seed_n ++ program_id ++ "ProgramDerivedAddress")
/// ```
///
/// # Errors
///
/// Returns [`CryptoError::InvalidSeedLength`] if any seed exceeds
/// [`MAX_SEED_LEN`] bytes, or [`CryptoError::MaxSeedLengthExceeded`] if
/// more than [`MAX_SEEDS`] seeds are provided.
pub fn create_program_address(seeds: &[&[u8]], program_id: &Hash) -> Result<Hash, CryptoError> {
    if seeds.len() > MAX_SEEDS {
        return Err(CryptoError::MaxSeedLengthExceeded {
            max: MAX_SEEDS,
            got: seeds.len(),
        });
    }

    for seed in seeds {
        if seed.len() > MAX_SEED_LEN {
            return Err(CryptoError::InvalidSeedLength {
                max: MAX_SEED_LEN,
                got: seed.len(),
            });
        }
    }

    // Length-prefix each seed to prevent concatenation ambiguity:
    // ["ab","c"] and ["a","bc"] must produce different addresses.
    let mut hasher = Hasher::new();
    for seed in seeds {
        hasher.update(&(seed.len() as u32).to_le_bytes());
        hasher.update(seed);
    }
    hasher.update(program_id.as_bytes());
    hasher.update(b"ProgramDerivedAddress");

    Ok(hasher.finalize())
}

/// Find a valid program-derived address by searching for a bump seed.
///
/// Tries bump values from 255 down to 0, appending each as a single-byte seed.
/// Returns the first derived address along with the bump seed that produced it.
///
/// In Nusantara's SHA3-512-based scheme every bump produces a valid hash (there
/// is no "off-curve" rejection like in elliptic-curve-based systems), so this always
/// returns with bump = 255. The function signature and bump-search pattern are
/// retained for API compatibility with Solana-style PDAs and to leave room for
/// future validity constraints.
///
/// # Panics
///
/// Panics if no valid address can be found after exhausting all 256 bump values.
/// This cannot happen with the current SHA3-512 scheme but is retained as a
/// safety invariant.
///
/// # Errors
///
/// Returns an error if the seeds violate length or count constraints (see
/// [`create_program_address`]).
pub fn find_program_address(seeds: &[&[u8]], program_id: &Hash) -> Result<(Hash, u8), CryptoError> {
    // Pre-validate seed constraints before the bump loop to fail fast with a
    // clear error instead of panicking.
    if seeds.len() >= MAX_SEEDS {
        return Err(CryptoError::MaxSeedLengthExceeded {
            max: MAX_SEEDS,
            // +1 because the bump seed will be appended
            got: seeds.len() + 1,
        });
    }

    for seed in seeds {
        if seed.len() > MAX_SEED_LEN {
            return Err(CryptoError::InvalidSeedLength {
                max: MAX_SEED_LEN,
                got: seed.len(),
            });
        }
    }

    for bump in (0..=255u8).rev() {
        let bump_slice: &[u8] = &[bump];
        let mut seeds_with_bump: Vec<&[u8]> = seeds.to_vec();
        seeds_with_bump.push(bump_slice);

        if let Ok(address) = create_program_address(&seeds_with_bump, program_id) {
            return Ok((address, bump));
        }
    }

    panic!("could not find program address: all 256 bump seeds exhausted")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::hash;

    #[test]
    fn create_pda_deterministic() {
        let program_id = hash(b"my_program");
        let seeds: &[&[u8]] = &[b"hello", b"world"];

        let pda1 = create_program_address(seeds, &program_id).unwrap();
        let pda2 = create_program_address(seeds, &program_id).unwrap();

        assert_eq!(pda1, pda2, "PDA derivation must be deterministic");
    }

    #[test]
    fn create_pda_different_seeds_different_address() {
        let program_id = hash(b"my_program");

        let pda1 = create_program_address(&[b"seed_a"], &program_id).unwrap();
        let pda2 = create_program_address(&[b"seed_b"], &program_id).unwrap();

        assert_ne!(pda1, pda2, "different seeds must produce different PDAs");
    }

    #[test]
    fn create_pda_different_program_different_address() {
        let program_a = hash(b"program_a");
        let program_b = hash(b"program_b");
        let seeds: &[&[u8]] = &[b"same_seed"];

        let pda_a = create_program_address(seeds, &program_a).unwrap();
        let pda_b = create_program_address(seeds, &program_b).unwrap();

        assert_ne!(
            pda_a, pda_b,
            "same seeds with different programs must produce different PDAs"
        );
    }

    #[test]
    fn create_pda_empty_seeds() {
        let program_id = hash(b"my_program");
        let pda = create_program_address(&[], &program_id).unwrap();

        // With no seeds, it should still produce a valid hash
        assert_ne!(pda, Hash::zero());
    }

    #[test]
    fn create_pda_max_seed_length() {
        let program_id = hash(b"my_program");
        let seed = [0xABu8; MAX_SEED_LEN]; // exactly 32 bytes

        let result = create_program_address(&[&seed], &program_id);
        assert!(
            result.is_ok(),
            "seed at exactly MAX_SEED_LEN should succeed"
        );
    }

    #[test]
    fn create_pda_seed_too_long() {
        let program_id = hash(b"my_program");
        let seed = [0u8; MAX_SEED_LEN + 1]; // 33 bytes

        let result = create_program_address(&[&seed], &program_id);
        assert!(result.is_err(), "seed exceeding MAX_SEED_LEN should fail");
        match result.unwrap_err() {
            CryptoError::InvalidSeedLength { max, got } => {
                assert_eq!(max, MAX_SEED_LEN);
                assert_eq!(got, MAX_SEED_LEN + 1);
            }
            other => panic!("expected InvalidSeedLength, got: {other}"),
        }
    }

    #[test]
    fn create_pda_too_many_seeds() {
        let program_id = hash(b"my_program");
        let seed = b"x";
        let seeds: Vec<&[u8]> = (0..MAX_SEEDS + 1).map(|_| seed.as_slice()).collect();

        let result = create_program_address(&seeds, &program_id);
        assert!(result.is_err(), "too many seeds should fail");
        assert!(matches!(
            result.unwrap_err(),
            CryptoError::MaxSeedLengthExceeded { .. }
        ));
    }

    #[test]
    fn create_pda_max_seeds_allowed() {
        let program_id = hash(b"my_program");
        let seed = b"x";
        let seeds: Vec<&[u8]> = (0..MAX_SEEDS).map(|_| seed.as_slice()).collect();

        let result = create_program_address(&seeds, &program_id);
        assert!(result.is_ok(), "exactly MAX_SEEDS seeds should succeed");
    }

    #[test]
    fn find_pda_returns_valid_address() {
        let program_id = hash(b"my_program");
        let seeds: &[&[u8]] = &[b"hello"];

        let (pda, bump) = find_program_address(seeds, &program_id).unwrap();

        // Verify the returned PDA matches create_program_address with the bump
        let bump_seed = [bump];
        let seeds_with_bump: Vec<&[u8]> = vec![b"hello", &bump_seed];
        let expected = create_program_address(&seeds_with_bump, &program_id).unwrap();
        assert_eq!(
            pda, expected,
            "find_program_address result must match create_program_address"
        );
    }

    #[test]
    fn find_pda_deterministic() {
        let program_id = hash(b"my_program");
        let seeds: &[&[u8]] = &[b"account", b"data"];

        let (pda1, bump1) = find_program_address(seeds, &program_id).unwrap();
        let (pda2, bump2) = find_program_address(seeds, &program_id).unwrap();

        assert_eq!(pda1, pda2);
        assert_eq!(bump1, bump2);
    }

    #[test]
    fn find_pda_starts_at_255() {
        // In the SHA3-512 scheme, every bump is valid, so find_program_address
        // should always return bump = 255 (the first one tried).
        let program_id = hash(b"my_program");
        let seeds: &[&[u8]] = &[b"test"];

        let (_pda, bump) = find_program_address(seeds, &program_id).unwrap();
        assert_eq!(bump, 255, "with SHA3-512, bump should always be 255");
    }

    #[test]
    fn find_pda_seed_too_long() {
        let program_id = hash(b"my_program");
        let seed = [0u8; MAX_SEED_LEN + 1];

        let result = find_program_address(&[&seed], &program_id);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            CryptoError::InvalidSeedLength { .. }
        ));
    }

    #[test]
    fn find_pda_too_many_seeds() {
        let program_id = hash(b"my_program");
        let seed = b"x";
        // MAX_SEEDS - 1 seeds is the maximum because find_program_address adds the bump
        let seeds: Vec<&[u8]> = (0..MAX_SEEDS).map(|_| seed.as_slice()).collect();

        let result = find_program_address(&seeds, &program_id);
        assert!(
            result.is_err(),
            "MAX_SEEDS seeds should fail because bump adds one more"
        );
        assert!(matches!(
            result.unwrap_err(),
            CryptoError::MaxSeedLengthExceeded { .. }
        ));
    }

    #[test]
    fn find_pda_max_seeds_minus_one_allowed() {
        let program_id = hash(b"my_program");
        let seed = b"x";
        // MAX_SEEDS - 1 seeds + bump = MAX_SEEDS total
        let seeds: Vec<&[u8]> = (0..MAX_SEEDS - 1).map(|_| seed.as_slice()).collect();

        let result = find_program_address(&seeds, &program_id);
        assert!(
            result.is_ok(),
            "MAX_SEEDS - 1 seeds should succeed (room for bump)"
        );
    }

    #[test]
    fn pda_is_64_bytes() {
        let program_id = hash(b"my_program");
        let pda = create_program_address(&[b"test"], &program_id).unwrap();
        assert_eq!(pda.as_bytes().len(), 64, "PDA must be 64 bytes (SHA3-512)");
    }

    #[test]
    fn pda_with_numeric_seed() {
        let program_id = hash(b"token_program");
        let account_index: u64 = 42;
        let index_bytes = account_index.to_le_bytes();

        let pda = create_program_address(&[b"mint", &index_bytes], &program_id).unwrap();
        assert_ne!(pda, Hash::zero());
    }

    #[test]
    fn pda_with_hash_seed() {
        let program_id = hash(b"my_program");
        let user_key = hash(b"user_pubkey");

        // Hash is 64 bytes, which exceeds MAX_SEED_LEN (32)
        let result = create_program_address(&[user_key.as_bytes()], &program_id);
        assert!(
            result.is_err(),
            "using a full 64-byte hash as seed should fail"
        );

        // Use first 32 bytes instead
        let truncated = &user_key.as_bytes()[..32];
        let result = create_program_address(&[truncated], &program_id);
        assert!(result.is_ok(), "truncated 32-byte seed should succeed");
    }

    #[test]
    fn pda_seed_order_matters() {
        let program_id = hash(b"my_program");

        let pda1 = create_program_address(&[b"a", b"b"], &program_id).unwrap();
        let pda2 = create_program_address(&[b"b", b"a"], &program_id).unwrap();

        assert_ne!(pda1, pda2, "seed order must affect the derived address");
    }

    #[test]
    fn pda_seed_concatenation_ambiguity_resolved() {
        let program_id = hash(b"my_program");

        // These two seed sets have identical raw concatenation ("abc")
        // but must produce different PDAs due to length-prefixing.
        let pda1 = create_program_address(&[b"ab", b"c"], &program_id).unwrap();
        let pda2 = create_program_address(&[b"a", b"bc"], &program_id).unwrap();

        assert_ne!(
            pda1, pda2,
            "different seed splits of the same bytes must produce different PDAs"
        );
    }
}
