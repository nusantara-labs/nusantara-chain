use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_core::batch_transaction::SignedTransactionBatch;
use nusantara_core::native_token::const_parse_u64;
use nusantara_core::transaction::Transaction;

use crate::compression;
use crate::error::TpuError;

pub const MAX_TRANSACTION_SIZE: u64 = const_parse_u64(env!("NUSA_TPU_MAX_TRANSACTION_SIZE"));
pub const MAX_UNSIGNED_BATCH_TXS: u64 = const_parse_u64(env!("NUSA_TPU_MAX_UNSIGNED_BATCH_TXS"));
pub const MAX_BATCH_ENTRIES: u64 = const_parse_u64(env!("NUSA_TPU_MAX_BATCH_ENTRIES"));

/// Decompressed size limit for a single-transaction message.
const MAX_SINGLE_DECOMPRESSED: usize = MAX_TRANSACTION_SIZE as usize;

/// Decompressed size limit for any batch message.
/// `MAX_TRANSACTION_SIZE * MAX_BATCH_ENTRIES` provides a per-message ceiling
/// that is independent of the compressed wire size — prevents compression bypass.
const MAX_BATCH_DECOMPRESSED: usize = MAX_TRANSACTION_SIZE as usize * MAX_BATCH_ENTRIES as usize;

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum TpuMessage {
    /// A single signed transaction.
    Transaction(Box<Transaction>),
    /// An unsigned batch forwarded by a downstream TPU relay.
    /// Capped at `MAX_UNSIGNED_BATCH_TXS` entries to prevent forced crypto work.
    TransactionBatch(Vec<Transaction>),
    /// A batch with a single Dilithium3 signature over a Merkle root.
    SignedBatch(Box<SignedTransactionBatch>),
}

impl TpuMessage {
    pub fn serialize_to_bytes(&self) -> Result<Vec<u8>, TpuError> {
        let raw = borsh::to_vec(self).map_err(|e| TpuError::Serialization(e.to_string()))?;
        compression::compress(&raw)
    }

    /// Deserialize a wire message, enforcing per-variant decompressed size limits.
    ///
    /// Returns `(message, decompressed_byte_len)` so callers can pass the exact
    /// decompressed size to downstream validators without re-estimating from the
    /// compressed wire size (which is always wrong on at least one side).
    ///
    /// The decompressed bound is derived from the message type, NOT from the
    /// compressed wire size — this closes the compression bypass bug where a
    /// 65 KB compressed stream could inflate to ~327 KB and pass size checks.
    ///
    /// Decompression ceiling selection (Risk 11):
    /// Borsh encodes enum discriminants as the first raw byte (u8, declaration order):
    ///   0 => Transaction   — tightest ceiling: MAX_SINGLE_DECOMPRESSED
    ///   1 => TransactionBatch — batch ceiling: MAX_BATCH_DECOMPRESSED
    ///   2 => SignedBatch      — batch ceiling: MAX_BATCH_DECOMPRESSED
    ///
    /// We decompress just enough to read that first discriminant byte, then
    /// decompress the full payload with the appropriate ceiling.  This avoids
    /// allocating a ~4 MB buffer for a single-transaction message (the previous
    /// behavior always used MAX_BATCH_DECOMPRESSED regardless of variant).
    pub fn deserialize_from_bytes(bytes: &[u8]) -> Result<(Self, usize), TpuError> {
        // Peek the discriminant: decompress at most 1 byte to read variant index.
        // If the payload is smaller than MAX_SINGLE_DECOMPRESSED we will
        // decompress it fully in the next step anyway, so this is cheap.
        let discriminant_buf = compression::decompress(bytes, 1).map_err(|e| match e {
            TpuError::Decompression(msg) => TpuError::Decompression(msg),
            other => other,
        });

        // Select the tightest decompressed-size ceiling based on the discriminant.
        //
        // The peek is best-effort: zstd and similar stream codecs may require the
        // entire compressed input before emitting even the first output byte, so
        // `decompress(bytes, 1)` can legitimately return an error or an empty
        // slice on a valid payload.  On any peek failure the code intentionally
        // falls back to `MAX_BATCH_DECOMPRESSED` as the safe ceiling — it is
        // always large enough for any valid message variant and prevents
        // under-allocation from causing a spurious rejection.  The subsequent
        // per-variant size enforcement (below) then applies the tightest bound
        // after the payload is fully decoded.
        let max_decompressed = match discriminant_buf.as_deref() {
            Ok([0, ..]) => MAX_SINGLE_DECOMPRESSED, // Transaction variant — tight ceiling
            // Batch variants, or peek returned Err/empty — safe fallback ceiling.
            // INTENTIONAL: never tighten this arm without verifying the codec
            // guarantees at least 1 byte of output from a partial read.
            _ => MAX_BATCH_DECOMPRESSED,
        };

        let raw = compression::decompress(bytes, max_decompressed).map_err(|e| match e {
            TpuError::Decompression(msg) => TpuError::Decompression(msg),
            other => other,
        })?;

        let decompressed_len = raw.len();

        let msg: Self =
            borsh::from_slice(&raw).map_err(|e| TpuError::Deserialization(e.to_string()))?;

        // Per-variant size enforcement after deserialization so we use the tightest
        // possible bound for each message type.
        match &msg {
            Self::Transaction(_) => {
                if decompressed_len > MAX_SINGLE_DECOMPRESSED {
                    return Err(TpuError::TransactionTooLarge {
                        size: decompressed_len,
                        max_size: MAX_SINGLE_DECOMPRESSED,
                    });
                }
            }
            Self::TransactionBatch(txs) => {
                if txs.len() > MAX_UNSIGNED_BATCH_TXS as usize {
                    return Err(TpuError::InvalidBatch(format!(
                        "unsigned batch has {} entries, max {}",
                        txs.len(),
                        MAX_UNSIGNED_BATCH_TXS
                    )));
                }
                if decompressed_len > MAX_BATCH_DECOMPRESSED {
                    return Err(TpuError::TransactionTooLarge {
                        size: decompressed_len,
                        max_size: MAX_BATCH_DECOMPRESSED,
                    });
                }
            }
            Self::SignedBatch(batch) => {
                if batch.entries.len() > MAX_BATCH_ENTRIES as usize {
                    return Err(TpuError::InvalidBatch(format!(
                        "signed batch has {} entries, max {}",
                        batch.entries.len(),
                        MAX_BATCH_ENTRIES
                    )));
                }
                if decompressed_len > MAX_BATCH_DECOMPRESSED {
                    return Err(TpuError::TransactionTooLarge {
                        size: decompressed_len,
                        max_size: MAX_BATCH_DECOMPRESSED,
                    });
                }
            }
        }

        Ok((msg, decompressed_len))
    }

    /// Consume this message and return the contained transactions.
    pub fn into_transactions(self) -> Vec<Transaction> {
        match self {
            Self::Transaction(tx) => vec![*tx],
            Self::TransactionBatch(txs) => txs,
            Self::SignedBatch(batch) => batch.to_transactions(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_core::message::Message;
    use nusantara_crypto::hash;

    fn test_tx() -> Transaction {
        let msg = Message {
            num_required_signatures: 1,
            num_readonly_signed: 0,
            num_readonly_unsigned: 1,
            account_keys: vec![hash(b"payer"), hash(b"program")],
            recent_blockhash: hash(b"blockhash"),
            instructions: vec![],
        };
        Transaction::new(msg)
    }

    #[test]
    fn single_tx_roundtrip() {
        let msg = TpuMessage::Transaction(Box::new(test_tx()));
        let bytes = msg.serialize_to_bytes().unwrap();
        let (decoded, decompressed_len) = TpuMessage::deserialize_from_bytes(&bytes).unwrap();
        assert_eq!(msg, decoded);
        assert!(decompressed_len > 0);
    }

    #[test]
    fn batch_roundtrip() {
        let msg = TpuMessage::TransactionBatch(vec![test_tx(), test_tx()]);
        let bytes = msg.serialize_to_bytes().unwrap();
        let (decoded, decompressed_len) = TpuMessage::deserialize_from_bytes(&bytes).unwrap();
        assert_eq!(msg, decoded);
        assert!(decompressed_len > 0);
    }

    #[test]
    fn into_transactions_single() {
        let tx = test_tx();
        let msg = TpuMessage::Transaction(Box::new(tx));
        assert_eq!(msg.into_transactions().len(), 1);
    }

    #[test]
    fn into_transactions_batch() {
        let msg = TpuMessage::TransactionBatch(vec![test_tx(), test_tx()]);
        assert_eq!(msg.into_transactions().len(), 2);
    }

    #[test]
    fn unsigned_batch_too_many_entries_rejected() {
        // Build a batch exceeding MAX_UNSIGNED_BATCH_TXS by injecting raw borsh.
        // Serialize a batch with MAX+1 entries, manually decompress-check path.
        let count = MAX_UNSIGNED_BATCH_TXS as usize + 1;
        let txs: Vec<Transaction> = (0..count).map(|_| test_tx()).collect();
        let msg = TpuMessage::TransactionBatch(txs);
        // Serialize bypassing our own guard (raw borsh + compress)
        let raw = borsh::to_vec(&msg).unwrap();
        let wire = crate::compression::compress(&raw).unwrap();
        let err = TpuMessage::deserialize_from_bytes(&wire).unwrap_err();
        assert!(
            err.to_string().contains("max"),
            "expected batch size error, got: {err}"
        );
    }

    #[test]
    fn config_values() {
        assert_eq!(MAX_TRANSACTION_SIZE, 65536);
        assert_eq!(MAX_UNSIGNED_BATCH_TXS, 64);
        assert_eq!(MAX_BATCH_ENTRIES, 64);
    }
}
