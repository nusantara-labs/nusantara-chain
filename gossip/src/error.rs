use thiserror::Error;

#[derive(Debug, Error)]
pub enum GossipError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("signature verification failed for {identity}")]
    SignatureVerification { identity: String },

    #[error("duplicate CRDS value: {label}")]
    DuplicateValue { label: String },

    #[error("stale CRDS value: wallclock {value_wallclock} < existing {existing_wallclock}")]
    StaleValue {
        value_wallclock: u64,
        existing_wallclock: u64,
    },

    #[error("unknown peer: {identity}")]
    UnknownPeer { identity: String },

    #[error("ping verification failed")]
    PingVerificationFailed,

    #[error("channel send error: {0}")]
    ChannelSend(String),

    #[error("socket bind failed: {addr}: {source}")]
    SocketBind {
        addr: String,
        source: std::io::Error,
    },

    #[error("oversized CRDS value")]
    OversizedValue,
}
