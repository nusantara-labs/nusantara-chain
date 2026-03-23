use crate::account_id::AccountId;
use crate::error::CryptoError;
use crate::hash::Hash;
use crate::keypair::Keypair;
use crate::pubkey::PublicKey;
use crate::signature::Signature;

pub trait Signer {
    fn public_key(&self) -> &PublicKey;

    fn address(&self) -> Hash {
        self.public_key().to_address()
    }

    fn account_id(&self) -> AccountId {
        self.public_key().to_account_id()
    }

    fn sign(&self, message: &[u8]) -> Result<Signature, CryptoError>;
}

impl Signer for Keypair {
    fn public_key(&self) -> &PublicKey {
        self.public_key()
    }

    fn sign(&self, message: &[u8]) -> Result<Signature, CryptoError> {
        Ok(self.sign(message))
    }
}
