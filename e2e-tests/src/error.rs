use thiserror::Error;

#[derive(Debug, Error)]
pub enum E2eError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("RPC error ({status}): {body}")]
    Rpc { status: u16, body: String },

    #[error("timeout: {0}")]
    Timeout(String),

    #[error("assertion failed: {0}")]
    Assertion(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("crypto error: {0}")]
    Crypto(String),

    #[error("cluster not ready: {0}")]
    ClusterNotReady(String),

    #[error("{0}")]
    Other(String),
}

impl From<nusantara_crypto::CryptoError> for E2eError {
    fn from(e: nusantara_crypto::CryptoError) -> Self {
        Self::Crypto(e.to_string())
    }
}

impl From<borsh::io::Error> for E2eError {
    fn from(e: borsh::io::Error) -> Self {
        Self::Serialization(e.to_string())
    }
}
