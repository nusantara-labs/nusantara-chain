use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_core::batch_transaction::SignedTransactionBatch;
use nusantara_core::transaction::Transaction;

use crate::compression;
use crate::tx_validator::MAX_TRANSACTION_SIZE;

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum TpuMessage {
    Transaction(Box<Transaction>),
    TransactionBatch(Vec<Transaction>),
    SignedBatch(Box<SignedTransactionBatch>),
}

impl TpuMessage {
    pub fn serialize_to_bytes(&self) -> Result<Vec<u8>, String> {
        let raw = borsh::to_vec(self).map_err(|e| e.to_string())?;
        compression::compress(&raw).map_err(|e| e.to_string())
    }

    pub fn deserialize_from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let max_size = bytes.len() * compression::MAX_DECOMPRESSION_RATIO
            + MAX_TRANSACTION_SIZE as usize;
        let raw = compression::decompress(bytes, max_size).map_err(|e| e.to_string())?;
        borsh::from_slice(&raw).map_err(|e| e.to_string())
    }

    pub fn transactions(&self) -> Vec<Transaction> {
        match self {
            Self::Transaction(tx) => vec![(**tx).clone()],
            Self::TransactionBatch(txs) => txs.clone(),
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
        let decoded = TpuMessage::deserialize_from_bytes(&bytes).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn batch_roundtrip() {
        let msg = TpuMessage::TransactionBatch(vec![test_tx(), test_tx()]);
        let bytes = msg.serialize_to_bytes().unwrap();
        let decoded = TpuMessage::deserialize_from_bytes(&bytes).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn transactions_extraction() {
        let tx = test_tx();
        let msg = TpuMessage::Transaction(Box::new(tx.clone()));
        assert_eq!(msg.transactions().len(), 1);

        let batch = TpuMessage::TransactionBatch(vec![tx.clone(), tx]);
        assert_eq!(batch.transactions().len(), 2);
    }
}
