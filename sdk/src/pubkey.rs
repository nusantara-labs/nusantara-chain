//! 64-byte public key type matching `nusantara_crypto::Hash`.
//!
//! Nusantara uses SHA3-512 (64-byte output) for all hashing, so public keys,
//! account addresses, and program IDs are all 64 bytes. This differs from
//! Solana's 32-byte model and is a deliberate design choice for post-quantum
//! security margin.

use borsh::{BorshDeserialize, BorshSerialize};

/// A 64-byte public key, address, or hash.
///
/// This is the SDK-side counterpart of `nusantara_crypto::Hash`. It uses the
/// same 64-byte representation so that borsh-serialized data is wire-compatible
/// between the SDK (compiled to WASM) and the validator (compiled natively).
#[derive(Clone, Copy, PartialEq, Eq, Hash, BorshSerialize, BorshDeserialize)]
pub struct Pubkey(pub [u8; 64]);

impl Default for Pubkey {
    fn default() -> Self {
        Self([0u8; 64])
    }
}

impl Pubkey {
    /// Size of a public key in bytes.
    pub const LEN: usize = 64;

    /// Create a `Pubkey` from a raw 64-byte array.
    pub const fn new(bytes: [u8; 64]) -> Self {
        Self(bytes)
    }

    /// The all-zeros key, used as a sentinel / default.
    pub const fn zero() -> Self {
        Self([0u8; 64])
    }

    /// View the underlying bytes.
    pub const fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }

    /// Construct from a byte slice, returning an error if the length is wrong.
    pub fn from_slice(data: &[u8]) -> Result<Self, &'static str> {
        if data.len() != 64 {
            return Err("pubkey must be 64 bytes");
        }
        let mut bytes = [0u8; 64];
        bytes.copy_from_slice(data);
        Ok(Self(bytes))
    }

    /// Maximum length of a single PDA seed in bytes.
    pub const MAX_SEED_LEN: usize = 32;

    /// Maximum number of seeds in a PDA derivation.
    pub const MAX_SEEDS: usize = 16;

    /// Create a program-derived address from seeds and a program ID.
    ///
    /// Under WASM this calls the `nusa_create_program_address` syscall.
    /// On native targets it uses the same SHA3-512-based scheme as the
    /// validator: `SHA3-512(seed_0 ++ ... ++ seed_n ++ program_id ++ "ProgramDerivedAddress")`.
    ///
    /// # Errors
    ///
    /// Returns `Err` if any seed exceeds [`Self::MAX_SEED_LEN`] bytes or if
    /// more than [`Self::MAX_SEEDS`] seeds are provided.
    pub fn create_program_address(
        seeds: &[&[u8]],
        program_id: &Pubkey,
    ) -> Result<Pubkey, &'static str> {
        if seeds.len() > Self::MAX_SEEDS {
            return Err("too many seeds");
        }
        for seed in seeds {
            if seed.len() > Self::MAX_SEED_LEN {
                return Err("seed too long");
            }
        }

        #[cfg(target_arch = "wasm32")]
        {
            let mut result = [0u8; 64];
            let mut seed_buf = Vec::new();
            for seed in seeds {
                seed_buf.extend_from_slice(&(seed.len() as u32).to_le_bytes());
                seed_buf.extend_from_slice(seed);
            }
            let ret = unsafe {
                crate::syscall::nusa_create_program_address(
                    seed_buf.as_ptr(),
                    seed_buf.len() as i32,
                    seeds.len() as i32,
                    program_id.as_bytes().as_ptr(),
                    result.as_mut_ptr(),
                )
            };
            if ret == 0 {
                Ok(Pubkey::new(result))
            } else {
                Err("create_program_address syscall failed")
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            use sha3::{Digest, Sha3_512};
            let mut hasher = Sha3_512::new();
            for seed in seeds {
                hasher.update(seed);
            }
            hasher.update(program_id.as_bytes());
            hasher.update(b"ProgramDerivedAddress");
            let result = hasher.finalize();
            let mut bytes = [0u8; 64];
            bytes.copy_from_slice(&result);
            Ok(Pubkey::new(bytes))
        }
    }

    /// Find a valid program-derived address by searching for a bump seed.
    pub fn find_program_address(
        seeds: &[&[u8]],
        program_id: &Pubkey,
    ) -> Result<(Pubkey, u8), &'static str> {
        if seeds.len() >= Self::MAX_SEEDS {
            return Err("too many seeds (need room for bump)");
        }
        for seed in seeds {
            if seed.len() > Self::MAX_SEED_LEN {
                return Err("seed too long");
            }
        }

        for bump in (0..=255u8).rev() {
            let bump_slice: &[u8] = &[bump];
            let mut seeds_with_bump: Vec<&[u8]> = seeds.to_vec();
            seeds_with_bump.push(bump_slice);

            if let Ok(address) = Self::create_program_address(&seeds_with_bump, program_id) {
                return Ok((address, bump));
            }
        }

        Err("could not find program address")
    }
}

impl core::fmt::Debug for Pubkey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Pubkey({:02x}{:02x}{:02x}{:02x}..)",
            self.0[0], self.0[1], self.0[2], self.0[3]
        )
    }
}

impl core::fmt::Display for Pubkey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Show the first 8 bytes in hex for human readability.
        for byte in &self.0[..8] {
            write!(f, "{byte:02x}")?;
        }
        write!(f, "...")
    }
}

impl AsRef<[u8]> for Pubkey {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Declare a program ID from a string seed.
///
/// This macro creates a module-level `id()` function that returns a deterministic
/// `Pubkey` derived from the given seed string. The derivation uses an XOR-fold
/// that matches the discriminator scheme in the `#[program]` macro; in production
/// the validator resolves program IDs via the loader, so this is primarily for
/// testing and self-identification.
///
/// # Usage
///
/// ```ignore
/// declare_id!("my_counter_program");
/// ```
#[macro_export]
macro_rules! declare_id {
    ($seed:expr) => {
        /// The program ID for this program.
        pub fn id() -> $crate::pubkey::Pubkey {
            let seed_bytes = $seed.as_bytes();
            let mut hash = [0u8; 64];
            let mut i = 0;
            while i < seed_bytes.len() {
                hash[i % 64] ^= seed_bytes[i];
                i += 1;
            }
            $crate::pubkey::Pubkey::new(hash)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pubkey_zero() {
        let z = Pubkey::zero();
        assert_eq!(z.0, [0u8; 64]);
    }

    #[test]
    fn pubkey_from_slice() {
        let bytes = [42u8; 64];
        let pk = Pubkey::from_slice(&bytes).unwrap();
        assert_eq!(pk.0, bytes);
    }

    #[test]
    fn pubkey_from_slice_wrong_len() {
        assert!(Pubkey::from_slice(&[0u8; 32]).is_err());
    }

    #[test]
    fn pubkey_equality() {
        let a = Pubkey::new([1u8; 64]);
        let b = Pubkey::new([1u8; 64]);
        let c = Pubkey::new([2u8; 64]);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn pubkey_debug_format() {
        let pk = Pubkey::new([0xAB; 64]);
        let debug = format!("{pk:?}");
        assert!(debug.contains("abababab"));
    }

    #[test]
    fn pubkey_display_format() {
        let pk = Pubkey::new([0xFF; 64]);
        let display = format!("{pk}");
        assert_eq!(display, "ffffffffffffffff...");
    }

    #[test]
    fn pubkey_borsh_roundtrip() {
        let pk = Pubkey::new([99u8; 64]);
        let encoded = borsh::to_vec(&pk).unwrap();
        assert_eq!(encoded.len(), 64);
        let decoded: Pubkey = borsh::from_slice(&encoded).unwrap();
        assert_eq!(pk, decoded);
    }

    #[test]
    fn pubkey_as_ref() {
        let pk = Pubkey::new([7u8; 64]);
        let slice: &[u8] = pk.as_ref();
        assert_eq!(slice.len(), 64);
        assert_eq!(slice[0], 7);
    }

    #[test]
    fn declare_id_produces_deterministic_key() {
        declare_id!("test_program");
        let a = id();
        let b = id();
        assert_eq!(a, b);
        // Should not be all zeros since the seed is non-empty.
        assert_ne!(a, Pubkey::zero());
    }

    #[test]
    fn create_pda_deterministic() {
        let program_id = Pubkey::new([1u8; 64]);
        let pda1 = Pubkey::create_program_address(&[b"hello"], &program_id).unwrap();
        let pda2 = Pubkey::create_program_address(&[b"hello"], &program_id).unwrap();
        assert_eq!(pda1, pda2);
    }

    #[test]
    fn create_pda_different_seeds() {
        let program_id = Pubkey::new([1u8; 64]);
        let pda1 = Pubkey::create_program_address(&[b"seed_a"], &program_id).unwrap();
        let pda2 = Pubkey::create_program_address(&[b"seed_b"], &program_id).unwrap();
        assert_ne!(pda1, pda2);
    }

    #[test]
    fn create_pda_seed_too_long() {
        let program_id = Pubkey::new([1u8; 64]);
        let seed = [0u8; 33]; // exceeds MAX_SEED_LEN
        let result = Pubkey::create_program_address(&[&seed], &program_id);
        assert!(result.is_err());
    }

    #[test]
    fn create_pda_too_many_seeds() {
        let program_id = Pubkey::new([1u8; 64]);
        let seed = b"x";
        let seeds: Vec<&[u8]> = (0..17).map(|_| seed.as_slice()).collect();
        let result = Pubkey::create_program_address(&seeds, &program_id);
        assert!(result.is_err());
    }

    #[test]
    fn find_pda_deterministic() {
        let program_id = Pubkey::new([2u8; 64]);
        let (pda1, bump1) = Pubkey::find_program_address(&[b"test"], &program_id).unwrap();
        let (pda2, bump2) = Pubkey::find_program_address(&[b"test"], &program_id).unwrap();
        assert_eq!(pda1, pda2);
        assert_eq!(bump1, bump2);
    }

    #[test]
    fn find_pda_matches_create() {
        let program_id = Pubkey::new([3u8; 64]);
        let (pda, bump) = Pubkey::find_program_address(&[b"account"], &program_id).unwrap();

        let bump_seed = [bump];
        let expected =
            Pubkey::create_program_address(&[b"account", &bump_seed], &program_id).unwrap();
        assert_eq!(pda, expected);
    }

    #[test]
    fn find_pda_too_many_seeds() {
        let program_id = Pubkey::new([1u8; 64]);
        let seed = b"x";
        let seeds: Vec<&[u8]> = (0..16).map(|_| seed.as_slice()).collect();
        let result = Pubkey::find_program_address(&seeds, &program_id);
        assert!(result.is_err());
    }

    #[test]
    fn pda_not_zero() {
        let program_id = Pubkey::new([1u8; 64]);
        let pda = Pubkey::create_program_address(&[b"any"], &program_id).unwrap();
        assert_ne!(pda, Pubkey::zero());
    }
}
