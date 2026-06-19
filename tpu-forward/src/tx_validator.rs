use nusantara_core::batch_transaction::SignedTransactionBatch;
use nusantara_core::transaction::Transaction;

use crate::error::TpuError;
use crate::protocol::{MAX_BATCH_ENTRIES, MAX_TRANSACTION_SIZE};

/// Absolute wire-size ceiling for any batch message (decompressed borsh bytes).
/// Used both here and in protocol::deserialize_from_bytes for consistency.
pub const MAX_BATCH_WIRE_SIZE: usize = MAX_TRANSACTION_SIZE as usize * MAX_BATCH_ENTRIES as usize;

/// Quick structural + signature validation of a single transaction.
///
/// `tx_serialized_size` must be the borsh-serialized byte length of `tx`,
/// not the compressed wire size — this prevents the compression bypass where
/// a 65 KB compressed stream inflates beyond MAX_TRANSACTION_SIZE.
pub fn validate(tx: &Transaction, tx_serialized_size: usize) -> Result<(), TpuError> {
    // 1. Cheap structural checks first — reject before any crypto.
    if tx.message.account_keys.is_empty() {
        return Err(TpuError::InvalidTransaction(
            "transaction has no account keys".to_string(),
        ));
    }

    if tx.signatures.is_empty() {
        return Err(TpuError::InvalidTransaction(
            "transaction has no signatures".to_string(),
        ));
    }

    if tx.signatures.len() != tx.message.num_required_signatures as usize {
        return Err(TpuError::InvalidTransaction(format!(
            "signature count {} != num_required_signatures {}",
            tx.signatures.len(),
            tx.message.num_required_signatures
        )));
    }

    if tx.message.account_keys.len() < tx.message.num_required_signatures as usize {
        return Err(TpuError::InvalidTransaction(
            "fewer account keys than required signatures".to_string(),
        ));
    }

    for ix in &tx.message.instructions {
        if ix.program_id_index as usize >= tx.message.account_keys.len() {
            return Err(TpuError::InvalidTransaction(
                "instruction references invalid program_id_index".to_string(),
            ));
        }
    }

    if tx.signer_pubkeys.len() != tx.signatures.len() {
        return Err(TpuError::InvalidTransaction(format!(
            "signer_pubkeys count {} != signature count {}",
            tx.signer_pubkeys.len(),
            tx.signatures.len()
        )));
    }

    // 2. Size check uses the actual serialized tx bytes, not the compressed wire size.
    if tx_serialized_size > MAX_TRANSACTION_SIZE as usize {
        return Err(TpuError::TransactionTooLarge {
            size: tx_serialized_size,
            max_size: MAX_TRANSACTION_SIZE as usize,
        });
    }

    // 3. Expensive crypto last — only reached after cheap checks pass.
    if let Err(e) = tx.verify_signatures() {
        metrics::counter!("nusantara_tpu_signature_failures_total").increment(1);
        return Err(TpuError::InvalidTransaction(format!(
            "signature verification failed: {e}"
        )));
    }

    Ok(())
}

/// Validate a signed transaction batch.
///
/// Validation order: cheap structural → absolute size cap → entry count cap →
/// per-entry structural → 1 Dilithium3 verify → N Merkle proof verifies.
///
/// `decompressed_wire_size` is the byte length of the borsh-encoded batch
/// (after decompression) — used as an absolute ceiling before per-entry work.
pub fn validate_batch(
    batch: &SignedTransactionBatch,
    decompressed_wire_size: usize,
) -> Result<(), TpuError> {
    // 1. Empty check.
    if batch.entries.is_empty() {
        return Err(TpuError::InvalidBatch("empty batch".to_string()));
    }

    // 2. Absolute wire-size ceiling — attacker-controlled entries.len() cannot
    //    inflate this bound because it is a fixed const, not `entries.len() * MAX_TX`.
    if decompressed_wire_size > MAX_BATCH_WIRE_SIZE {
        return Err(TpuError::TransactionTooLarge {
            size: decompressed_wire_size,
            max_size: MAX_BATCH_WIRE_SIZE,
        });
    }

    // 3. Entry count cap — caps forced Merkle verify work.
    if batch.entries.len() > MAX_BATCH_ENTRIES as usize {
        return Err(TpuError::InvalidBatch(format!(
            "batch has {} entries, max {}",
            batch.entries.len(),
            MAX_BATCH_ENTRIES
        )));
    }

    // 4. Per-entry structural checks (no crypto yet).
    for (i, entry) in batch.entries.iter().enumerate() {
        if entry.message.account_keys.is_empty() {
            return Err(TpuError::InvalidBatch(format!(
                "batch entry {i}: no account keys"
            )));
        }
        for ix in &entry.message.instructions {
            if ix.program_id_index as usize >= entry.message.account_keys.len() {
                return Err(TpuError::InvalidBatch(format!(
                    "batch entry {i}: invalid program_id_index"
                )));
            }
        }
    }

    // 5. Expensive crypto — 1 Dilithium3 verify over the batch signature.
    if !batch.verify_signature() {
        metrics::counter!("nusantara_tpu_signature_failures_total").increment(1);
        return Err(TpuError::InvalidBatch(
            "batch signature verification failed".to_string(),
        ));
    }

    // 6. N Merkle proof verifications (cheapest crypto, still after structural).
    if !batch.verify_all() {
        // Distinct counter from signature_failures — different failure mode.
        metrics::counter!("nusantara_tpu_merkle_proof_failures_total").increment(1);
        return Err(TpuError::InvalidBatch(
            "batch merkle proof verification failed".to_string(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_core::instruction::CompiledInstruction;
    use nusantara_core::message::Message;
    use nusantara_crypto::{Keypair, hash};

    fn valid_tx() -> Transaction {
        let kp = Keypair::generate();
        let payer = kp.address();
        let program = hash(b"program");

        let msg = Message {
            num_required_signatures: 1,
            num_readonly_signed: 0,
            num_readonly_unsigned: 1,
            account_keys: vec![payer, program],
            recent_blockhash: hash(b"blockhash"),
            instructions: vec![CompiledInstruction {
                program_id_index: 1,
                accounts: vec![0],
                data: vec![42],
            }],
        };
        let mut tx = Transaction::new(msg);
        tx.sign(&[&kp]);
        tx
    }

    fn tx_serialized_size(tx: &Transaction) -> usize {
        borsh::to_vec(tx).unwrap().len()
    }

    #[test]
    fn config_values() {
        assert_eq!(MAX_TRANSACTION_SIZE, 65536);
        assert_eq!(MAX_BATCH_ENTRIES, 64);
    }

    #[test]
    fn valid_transaction_passes() {
        let tx = valid_tx();
        let size = tx_serialized_size(&tx);
        assert!(validate(&tx, size).is_ok());
    }

    #[test]
    fn no_signatures_fails() {
        let msg = Message {
            num_required_signatures: 1,
            num_readonly_signed: 0,
            num_readonly_unsigned: 0,
            account_keys: vec![hash(b"a")],
            recent_blockhash: hash(b"bh"),
            instructions: vec![],
        };
        let tx = Transaction::new(msg);
        assert!(validate(&tx, 100).is_err());
    }

    #[test]
    fn signature_count_mismatch() {
        let msg = Message {
            num_required_signatures: 2,
            num_readonly_signed: 0,
            num_readonly_unsigned: 0,
            account_keys: vec![hash(b"a"), hash(b"b")],
            recent_blockhash: hash(b"bh"),
            instructions: vec![],
        };
        let mut tx = Transaction::new(msg);
        let kp = Keypair::generate();
        tx.signatures = vec![kp.sign(&[0u8])];
        tx.signer_pubkeys = vec![kp.public_key().clone()];
        let err = validate(&tx, 100).unwrap_err().to_string();
        assert!(
            err.contains("signature count") && err.contains("num_required_signatures"),
            "expected signature count mismatch error, got: {err}"
        );
    }

    #[test]
    fn signer_pubkeys_count_mismatch() {
        let kp = Keypair::generate();
        let payer = kp.address();
        let program = hash(b"program");

        let msg = Message {
            num_required_signatures: 1,
            num_readonly_signed: 0,
            num_readonly_unsigned: 1,
            account_keys: vec![payer, program],
            recent_blockhash: hash(b"blockhash"),
            instructions: vec![CompiledInstruction {
                program_id_index: 1,
                accounts: vec![0],
                data: vec![42],
            }],
        };
        let mut tx = Transaction::new(msg);
        tx.sign(&[&kp]);
        tx.signer_pubkeys.clear();
        let err = validate(&tx, 100).unwrap_err().to_string();
        assert!(
            err.contains("signer_pubkeys count"),
            "expected signer_pubkeys count mismatch error, got: {err}"
        );
    }

    #[test]
    fn signature_verification_failure() {
        let kp = Keypair::generate();
        let kp2 = Keypair::generate();
        let payer = kp.address();
        let program = hash(b"program");

        let msg = Message {
            num_required_signatures: 1,
            num_readonly_signed: 0,
            num_readonly_unsigned: 1,
            account_keys: vec![payer, program],
            recent_blockhash: hash(b"blockhash"),
            instructions: vec![CompiledInstruction {
                program_id_index: 1,
                accounts: vec![0],
                data: vec![42],
            }],
        };
        let mut tx = Transaction::new(msg);
        tx.sign(&[&kp]);
        tx.signer_pubkeys = vec![kp2.public_key().clone()];
        let err = validate(&tx, 100).unwrap_err().to_string();
        assert!(
            err.contains("signature verification failed"),
            "expected signature verification error, got: {err}"
        );
    }

    #[test]
    fn invalid_program_id_index() {
        let kp = Keypair::generate();
        let msg = Message {
            num_required_signatures: 1,
            num_readonly_signed: 0,
            num_readonly_unsigned: 0,
            account_keys: vec![kp.address()],
            recent_blockhash: hash(b"bh"),
            instructions: vec![CompiledInstruction {
                program_id_index: 99,
                accounts: vec![],
                data: vec![],
            }],
        };
        let mut tx = Transaction::new(msg);
        tx.sign(&[&kp]);
        assert!(validate(&tx, 100).is_err());
    }

    #[test]
    fn rejects_oversized_tx() {
        let tx = valid_tx();
        let err = validate(&tx, MAX_TRANSACTION_SIZE as usize + 1).unwrap_err();
        assert!(err.to_string().contains("too large"));
    }

    #[test]
    fn rejects_oversized_batch_wire() {
        // Build a minimal valid batch using the proper constructor.
        let kp = Keypair::generate();
        let msg = Message {
            num_required_signatures: 1,
            num_readonly_signed: 0,
            num_readonly_unsigned: 0,
            account_keys: vec![kp.address()],
            recent_blockhash: hash(b"bh"),
            instructions: vec![],
        };
        let batch =
            SignedTransactionBatch::new(vec![msg], &kp).expect("failed to build test batch");
        // Pass a wire size exceeding MAX_BATCH_WIRE_SIZE — the absolute ceiling rejects it.
        let err = validate_batch(&batch, MAX_BATCH_WIRE_SIZE + 1).unwrap_err();
        assert!(err.to_string().contains("too large"));
    }
}
