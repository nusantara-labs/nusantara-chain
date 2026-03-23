use std::path::Path;

use nusantara_crypto::Keypair;

use crate::error::CliError;

use nusantara_crypto::pubkey::PUBLIC_KEY_BYTES;
use nusantara_crypto::keypair::SECRET_KEY_BYTES;

const KEYPAIR_SIZE: usize = PUBLIC_KEY_BYTES + SECRET_KEY_BYTES; // pubkey + secret

pub fn load_keypair(path: &str) -> Result<Keypair, CliError> {
    let expanded = shellexpand(path);
    let bytes = std::fs::read(&expanded)
        .map_err(|e| CliError::Keypair(format!("failed to read {expanded}: {e}")))?;

    if bytes.len() != KEYPAIR_SIZE {
        return Err(CliError::Keypair(format!(
            "invalid keypair file size: expected {KEYPAIR_SIZE}, got {}",
            bytes.len()
        )));
    }

    Keypair::from_bytes(&bytes[..PUBLIC_KEY_BYTES], &bytes[PUBLIC_KEY_BYTES..])
        .map_err(|e| CliError::Keypair(format!("invalid keypair: {e}")))
}

pub fn save_keypair(path: &str, keypair: &Keypair) -> Result<(), CliError> {
    let expanded = shellexpand(path);
    if let Some(parent) = Path::new(&expanded).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut bytes = Vec::with_capacity(KEYPAIR_SIZE);
    bytes.extend_from_slice(keypair.public_key().as_bytes());
    bytes.extend_from_slice(keypair.secret_key().as_bytes());
    std::fs::write(&expanded, &bytes)?;
    Ok(())
}

pub fn generate_keypair() -> Keypair {
    Keypair::generate()
}

fn shellexpand(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest).to_string_lossy().to_string();
    }
    path.to_string()
}
