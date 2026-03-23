#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("invalid hash length: expected {expected}, got {got}")]
    InvalidHashLength { expected: usize, got: usize },

    #[error("invalid public key length: expected {expected}, got {got}")]
    InvalidPublicKeyLength { expected: usize, got: usize },

    #[error("invalid secret key length: expected {expected}, got {got}")]
    InvalidSecretKeyLength { expected: usize, got: usize },

    #[error("invalid signature length: expected {expected}, got {got}")]
    InvalidSignatureLength { expected: usize, got: usize },

    #[error("signature verification failed")]
    VerificationFailed,

    #[error("invalid base64: {0}")]
    InvalidBase64(String),


    #[error("invalid account id: {0}")]
    InvalidAccountId(String),

    #[error("invalid key bytes")]
    InvalidKeyBytes,

    #[error("invalid seed length: each seed must be at most {max} bytes, got {got}")]
    InvalidSeedLength { max: usize, got: usize },

    #[error("max seed length exceeded: at most {max} seeds allowed, got {got}")]
    MaxSeedLengthExceeded { max: usize, got: usize },
}
