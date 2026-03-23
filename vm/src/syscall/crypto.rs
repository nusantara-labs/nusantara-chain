//! Cryptographic syscalls: SHA3-512 hashing and Dilithium3 signature verification.
//!
//! These allow WASM programs to perform cryptographic operations using the
//! same primitives as the rest of the Nusantara blockchain (SHA3-512 for
//! hashing, Dilithium3 for post-quantum signatures).

use nusantara_crypto::{Hash, PublicKey, Signature, create_program_address, hash};

use crate::config::{COST_CREATE_PROGRAM_ADDRESS, COST_SHA3_512_BASE, COST_SIGNATURE_VERIFY};
use crate::error::VmError;

/// Compute SHA3-512 hash of the input data.
pub fn sha3_512(data: &[u8]) -> Hash {
    hash(data)
}

/// Calculate the compute-unit cost for a SHA3-512 operation.
///
/// The cost scales linearly with input size: a base charge plus one unit per
/// 64 bytes of input (one SHA3 block).
pub fn sha3_512_cost(data_len: usize) -> u64 {
    COST_SHA3_512_BASE + (data_len as u64 / 64)
}

/// Verify a Dilithium3 detached signature.
///
/// Returns `Ok(true)` if the signature is valid, `Ok(false)` if it is
/// well-formed but does not match, or `Err` if the key/signature bytes
/// are malformed.
pub fn verify_signature(
    pubkey_bytes: &[u8],
    message: &[u8],
    signature_bytes: &[u8],
) -> Result<bool, VmError> {
    let pubkey = PublicKey::from_bytes(pubkey_bytes)
        .map_err(|e| VmError::Syscall(format!("invalid public key: {e}")))?;
    let signature = Signature::from_bytes(signature_bytes)
        .map_err(|e| VmError::Syscall(format!("invalid signature: {e}")))?;

    // Signature::verify returns Ok(()) on success, Err on failure
    match signature.verify(&pubkey, message) {
        Ok(()) => Ok(true),
        Err(_) => Ok(false),
    }
}

/// Fixed compute-unit cost for a Dilithium3 signature verification.
pub fn verify_signature_cost() -> u64 {
    COST_SIGNATURE_VERIFY
}

/// Derive a program address from seeds and a program ID.
///
/// This is the VM-side implementation of the `nusa_create_program_address`
/// syscall. It delegates to `nusantara_crypto::create_program_address` which
/// computes `SHA3-512(seeds ++ program_id ++ "ProgramDerivedAddress")`.
///
/// Returns the derived 64-byte `Hash` or a `VmError` if the seeds are invalid.
pub fn create_pda(seeds: &[&[u8]], program_id: &Hash) -> Result<Hash, VmError> {
    create_program_address(seeds, program_id)
        .map_err(|e| VmError::Syscall(format!("create_program_address failed: {e}")))
}

/// Fixed compute-unit cost for a PDA derivation.
pub fn create_pda_cost() -> u64 {
    COST_CREATE_PROGRAM_ADDRESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_deterministic() {
        let h1 = sha3_512(b"test");
        let h2 = sha3_512(b"test");
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_different_input() {
        let h1 = sha3_512(b"test1");
        let h2 = sha3_512(b"test2");
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_empty_input() {
        let h = sha3_512(b"");
        // SHA3-512 of empty input is well-defined; just verify it's not zero
        assert_ne!(h, Hash::zero());
    }

    #[test]
    fn cost_calculation() {
        assert_eq!(sha3_512_cost(0), COST_SHA3_512_BASE);
        assert_eq!(sha3_512_cost(64), COST_SHA3_512_BASE + 1);
        assert_eq!(sha3_512_cost(128), COST_SHA3_512_BASE + 2);
        // Truncated division: 63 bytes => 0 extra blocks
        assert_eq!(sha3_512_cost(63), COST_SHA3_512_BASE);
    }

    #[test]
    fn verify_signature_invalid_pubkey() {
        let bad_pubkey = vec![0u8; 100]; // wrong length
        let result = verify_signature(&bad_pubkey, b"msg", &[0u8; 100]);
        assert!(result.is_err());
    }

    #[test]
    fn verify_signature_cost_value() {
        assert_eq!(verify_signature_cost(), COST_SIGNATURE_VERIFY);
    }

    #[test]
    fn create_pda_deterministic() {
        let program_id = hash(b"test_program");
        let pda1 = create_pda(&[b"seed"], &program_id).unwrap();
        let pda2 = create_pda(&[b"seed"], &program_id).unwrap();
        assert_eq!(pda1, pda2);
    }

    #[test]
    fn create_pda_different_seeds() {
        let program_id = hash(b"test_program");
        let pda1 = create_pda(&[b"a"], &program_id).unwrap();
        let pda2 = create_pda(&[b"b"], &program_id).unwrap();
        assert_ne!(pda1, pda2);
    }

    #[test]
    fn create_pda_seed_too_long() {
        let program_id = hash(b"test_program");
        let long_seed = [0u8; 33];
        let result = create_pda(&[&long_seed], &program_id);
        assert!(result.is_err());
    }

    #[test]
    fn create_pda_cost_value() {
        assert_eq!(create_pda_cost(), COST_CREATE_PROGRAM_ADDRESS);
    }
}
