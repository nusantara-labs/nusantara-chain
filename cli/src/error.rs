#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("rpc error: {0}")]
    Rpc(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("config error: {0}")]
    Config(String),

    #[error("keypair error: {0}")]
    Keypair(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("{0}")]
    Other(String),
}
