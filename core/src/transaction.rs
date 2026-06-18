use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::{Hash, Keypair, PublicKey, Signature, hash as crypto_hash};

use crate::error::CoreError;
use crate::message::Message;

#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct Transaction {
    pub signatures: Vec<Signature>,
    pub signer_pubkeys: Vec<PublicKey>,
    pub message: Message,
    /// Cached transaction hash to avoid redundant SHA3-512 recomputation.
    /// Skipped in borsh serialization; excluded from equality checks.
    #[borsh(skip)]
    cached_hash: std::sync::OnceLock<Hash>,
}

impl PartialEq for Transaction {
    fn eq(&self, other: &Self) -> bool {
        self.signatures == other.signatures
            && self.signer_pubkeys == other.signer_pubkeys
            && self.message == other.message
    }
}

impl Eq for Transaction {}

impl Transaction {
    pub fn new(message: Message) -> Self {
        Self {
            signatures: Vec::new(),
            signer_pubkeys: Vec::new(),
            message,
            cached_hash: std::sync::OnceLock::new(),
        }
    }

    pub fn message_data(&self) -> Vec<u8> {
        borsh::to_vec(&self.message).expect("message serialization cannot fail")
    }

    pub fn sign(&mut self, keypairs: &[&Keypair]) {
        let message_bytes = self.message_data();
        self.signatures = keypairs
            .iter()
            .map(|kp| kp.sign(&message_bytes))
            .collect();
        self.signer_pubkeys = keypairs
            .iter()
            .map(|kp| kp.public_key().clone())
            .collect();
        // Invalidate cached hash since signatures changed
        self.cached_hash = std::sync::OnceLock::new();
    }

    pub fn verify(&self, pubkeys: &[PublicKey]) -> Result<(), CoreError> {
        if self.signatures.len() != pubkeys.len() {
            return Err(CoreError::InvalidTransaction(format!(
                "signature count {} != pubkey count {}",
                self.signatures.len(),
                pubkeys.len()
            )));
        }

        let message_bytes = self.message_data();
        for (i, (sig, pk)) in self.signatures.iter().zip(pubkeys.iter()).enumerate() {
            sig.verify(pk, &message_bytes).map_err(|_| {
                CoreError::InvalidTransaction(format!("signature {i} verification failed"))
            })?;
        }
        Ok(())
    }

    /// Verify signatures using the embedded signer public keys.
    pub fn verify_signatures(&self) -> Result<(), CoreError> {
        let required = self.message.num_required_signatures as usize;
        if self.signatures.len() != required {
            return Err(CoreError::InvalidTransaction(format!(
                "expected {} signatures, got {}",
                required,
                self.signatures.len()
            )));
        }
        if self.signer_pubkeys.len() != self.signatures.len() {
            return Err(CoreError::InvalidTransaction(format!(
                "signer_pubkeys count {} != signature count {}",
                self.signer_pubkeys.len(),
                self.signatures.len()
            )));
        }
        self.verify(&self.signer_pubkeys)?;

        // Bind signer identities to account keys: each signer's pubkey hash
        // must match the corresponding account_key to prevent a malicious signer
        // from operating on another user's accounts.
        if required > self.message.account_keys.len() {
            return Err(CoreError::InvalidTransaction(format!(
                "num_required_signatures {} exceeds account_keys count {}",
                required,
                self.message.account_keys.len()
            )));
        }
        for i in 0..required {
            let expected = self.message.account_keys[i];
            let actual = crypto_hash(self.signer_pubkeys[i].as_bytes());
            if expected != actual {
                return Err(CoreError::InvalidTransaction(format!(
                    "signer {i} pubkey hash does not match account_keys[{i}]"
                )));
            }
        }

        Ok(())
    }

    pub fn hash(&self) -> Hash {
        // Always hash the canonical message bytes so the transaction id is a
        // deterministic commitment to the message content.  Dilithium3 is
        // randomized, so hashing a signature would produce a different id each
        // time the same message is signed.
        *self.cached_hash.get_or_init(|| crypto_hash(&self.message_data()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instruction::{AccountMeta, CompiledInstruction, Instruction};
    use nusantara_crypto::hash;

    fn test_message() -> Message {
        Message {
            num_required_signatures: 1,
            num_readonly_signed: 0,
            num_readonly_unsigned: 1,
            account_keys: vec![hash(b"payer"), hash(b"program")],
            recent_blockhash: hash(b"blockhash"),
            instructions: vec![CompiledInstruction {
                program_id_index: 1,
                accounts: vec![0],
                data: vec![42],
            }],
        }
    }

    #[test]
    fn new_transaction_has_no_signatures() {
        let tx = Transaction::new(test_message());
        assert!(tx.signatures.is_empty());
        assert!(tx.signer_pubkeys.is_empty());
    }

    #[test]
    fn sign_and_verify() {
        let kp = Keypair::generate();
        let payer_addr = kp.address();
        let program = hash(b"program");

        let ix = Instruction {
            program_id: program,
            accounts: vec![AccountMeta::new(hash(b"account"), false)],
            data: vec![1],
        };
        let msg = Message::new(&[ix], &payer_addr).unwrap();
        let mut tx = Transaction::new(msg);
        tx.sign(&[&kp]);

        assert_eq!(tx.signatures.len(), 1);
        assert_eq!(tx.signer_pubkeys.len(), 1);
        tx.verify(&[kp.public_key().clone()]).unwrap();
        tx.verify_signatures().unwrap();
    }

    #[test]
    fn verify_signatures_rejects_invalid() {
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();
        let payer_addr = kp1.address();
        let program = hash(b"program");

        let ix = Instruction {
            program_id: program,
            accounts: vec![AccountMeta::new(hash(b"account"), false)],
            data: vec![1],
        };
        let msg = Message::new(&[ix], &payer_addr).unwrap();
        let mut tx = Transaction::new(msg);
        tx.sign(&[&kp1]);

        // Tamper: replace the pubkey with wrong one
        tx.signer_pubkeys = vec![kp2.public_key().clone()];
        assert!(tx.verify_signatures().is_err());
    }

    #[test]
    fn borsh_roundtrip() {
        let kp = Keypair::generate();
        let payer_addr = kp.address();
        let program = hash(b"program");

        let ix = Instruction {
            program_id: program,
            accounts: vec![AccountMeta::new(hash(b"account"), false)],
            data: vec![1],
        };
        let msg = Message::new(&[ix], &payer_addr).unwrap();
        let mut tx = Transaction::new(msg);
        tx.sign(&[&kp]);
        let encoded = borsh::to_vec(&tx).unwrap();
        let decoded: Transaction = borsh::from_slice(&encoded).unwrap();
        assert_eq!(tx, decoded);
        // Verify signatures survive roundtrip
        decoded.verify_signatures().unwrap();
    }

    #[test]
    fn verify_signatures_rejects_mismatched_pubkey_hash() {
        // Alice signs a transaction whose account_keys[0] is Bob's address.
        // The signature is valid for Alice's pubkey, but Alice's pubkey hash
        // does not match account_keys[0], so verify_signatures must reject it.
        let alice = Keypair::generate();
        let bob = Keypair::generate();
        let bob_addr = bob.address();
        let program = hash(b"program");

        let ix = Instruction {
            program_id: program,
            accounts: vec![AccountMeta::new(hash(b"account"), false)],
            data: vec![1],
        };
        // Build message with Bob as payer (account_keys[0] = bob_addr)
        let msg = Message::new(&[ix], &bob_addr).unwrap();
        let mut tx = Transaction::new(msg);

        // Sign with Alice's keypair -- the cryptographic signature is valid
        // for Alice's pubkey, but the account binding check must catch that
        // hash(alice.pubkey) != bob_addr.
        let message_bytes = tx.message_data();
        tx.signatures = vec![alice.sign(&message_bytes)];
        tx.signer_pubkeys = vec![alice.public_key().clone()];

        let err = tx.verify_signatures().unwrap_err();
        let err_msg = format!("{err}");
        assert!(
            err_msg.contains("pubkey hash does not match"),
            "expected pubkey-hash-mismatch error, got: {err_msg}"
        );
    }
}
