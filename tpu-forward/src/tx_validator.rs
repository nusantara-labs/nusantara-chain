use nusantara_core::batch_transaction::SignedTransactionBatch;
use nusantara_core::native_token::const_parse_u64;
use nusantara_core::transaction::Transaction;

use crate::error::TpuError;

pub const MAX_TRANSACTION_SIZE: u64 = const_parse_u64(env!("NUSA_TPU_MAX_TRANSACTION_SIZE"));

pub struct TxValidator;

impl TxValidator {
    /// Quick structural validation of a transaction.
    /// `raw_size` is the byte size already read from the QUIC stream, avoiding
    /// a redundant borsh re-serialization just to check size.
    pub fn validate(tx: &Transaction, raw_size: usize) -> Result<(), TpuError> {
        // Check that there's at least one signature
        if tx.signatures.is_empty() {
            return Err(TpuError::InvalidTransaction(
                "transaction has no signatures".to_string(),
            ));
        }

        // Check message has required fields
        if tx.message.account_keys.is_empty() {
            return Err(TpuError::InvalidTransaction(
                "transaction has no account keys".to_string(),
            ));
        }

        // Check signature count matches num_required_signatures
        if tx.signatures.len() != tx.message.num_required_signatures as usize {
            return Err(TpuError::InvalidTransaction(format!(
                "signature count {} != num_required_signatures {}",
                tx.signatures.len(),
                tx.message.num_required_signatures
            )));
        }

        // Check size using the raw bytes already read (no re-serialization needed)
        if raw_size > MAX_TRANSACTION_SIZE as usize {
            return Err(TpuError::TransactionTooLarge {
                size: raw_size,
                max_size: MAX_TRANSACTION_SIZE as usize,
            });
        }

        // Check account_keys length >= num_required_signatures
        if tx.message.account_keys.len() < tx.message.num_required_signatures as usize {
            return Err(TpuError::InvalidTransaction(
                "fewer account keys than required signatures".to_string(),
            ));
        }

        // Check instruction program_id_index references valid account
        for ix in &tx.message.instructions {
            if ix.program_id_index as usize >= tx.message.account_keys.len() {
                return Err(TpuError::InvalidTransaction(
                    "instruction references invalid program_id_index".to_string(),
                ));
            }
        }

        // Check signer_pubkeys count matches signatures
        if tx.signer_pubkeys.len() != tx.signatures.len() {
            return Err(TpuError::InvalidTransaction(format!(
                "signer_pubkeys count {} != signature count {}",
                tx.signer_pubkeys.len(),
                tx.signatures.len()
            )));
        }

        // Verify signatures (Dilithium3 verification at ingress)
        if let Err(e) = tx.verify_signatures() {
            metrics::counter!("nusantara_tpu_signature_failures_total").increment(1);
            return Err(TpuError::InvalidTransaction(format!(
                "signature verification failed: {e}"
            )));
        }

        Ok(())
    }

    /// Validate a signed transaction batch.
    /// 1 Dilithium3 verify + N Merkle proof verifies + structural checks.
    pub fn validate_batch(
        batch: &SignedTransactionBatch,
        raw_size: usize,
    ) -> Result<(), TpuError> {
        if batch.entries.is_empty() {
            return Err(TpuError::InvalidBatch("empty batch".to_string()));
        }

        if raw_size > MAX_TRANSACTION_SIZE as usize * batch.entries.len() {
            return Err(TpuError::TransactionTooLarge {
                size: raw_size,
                max_size: MAX_TRANSACTION_SIZE as usize * batch.entries.len(),
            });
        }

        // Verify batch signature (1 Dilithium3 verify)
        if !batch.verify_signature() {
            metrics::counter!("nusantara_tpu_signature_failures_total").increment(1);
            return Err(TpuError::InvalidBatch(
                "batch signature verification failed".to_string(),
            ));
        }

        // Verify all Merkle proofs
        if !batch.verify_all() {
            return Err(TpuError::InvalidBatch(
                "batch merkle proof verification failed".to_string(),
            ));
        }

        // Structural validation on each message
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

        Ok(())
    }
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

    /// Helper: get the borsh-serialized size of a transaction for the raw_size param.
    fn tx_raw_size(tx: &Transaction) -> usize {
        borsh::to_vec(tx).unwrap().len()
    }

    #[test]
    fn config_values() {
        assert_eq!(MAX_TRANSACTION_SIZE, 65536);
    }

    #[test]
    fn valid_transaction_passes() {
        let tx = valid_tx();
        let size = tx_raw_size(&tx);
        assert!(TxValidator::validate(&tx, size).is_ok());
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
        assert!(TxValidator::validate(&tx, 100).is_err());
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
        // Only add 1 signature when 2 required — keep signer_pubkeys in sync
        // so the test specifically hits the sig count vs num_required_signatures check
        let kp = Keypair::generate();
        tx.signatures = vec![kp.sign(&[0u8])];
        tx.signer_pubkeys = vec![kp.public_key().clone()];
        let err = TxValidator::validate(&tx, 100).unwrap_err().to_string();
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

        // Tamper: remove signer_pubkeys so count no longer matches signatures
        tx.signer_pubkeys.clear();
        let err = TxValidator::validate(&tx, 100).unwrap_err().to_string();
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

        // Tamper: replace signer_pubkeys with wrong key so verify_signatures fails
        tx.signer_pubkeys = vec![kp2.public_key().clone()];
        let err = TxValidator::validate(&tx, 100).unwrap_err().to_string();
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
                program_id_index: 99, // out of bounds
                accounts: vec![],
                data: vec![],
            }],
        };
        let mut tx = Transaction::new(msg);
        tx.sign(&[&kp]);
        assert!(TxValidator::validate(&tx, 100).is_err());
    }

    #[test]
    fn rejects_oversized_raw() {
        let tx = valid_tx();
        // Pass a raw_size exceeding the limit
        let err = TxValidator::validate(&tx, MAX_TRANSACTION_SIZE as usize + 1).unwrap_err();
        assert!(err.to_string().contains("too large"));
    }
}
