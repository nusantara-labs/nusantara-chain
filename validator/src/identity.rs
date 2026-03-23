use std::path::Path;

use nusantara_crypto::{Hash, Keypair};
use tracing::{info, warn};

use crate::cli::Cli;
use crate::constants::KEYPAIR_SIZE;
use crate::error::ValidatorError;

/// Write keypair bytes to a file with restricted permissions (0o600 on Unix).
fn write_keypair_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

pub(crate) fn load_or_generate_keypair(cli: &Cli) -> Result<Keypair, ValidatorError> {
    if let Some(path) = &cli.identity {
        // --identity flag: load from explicit path
        info!(path, "loading identity keypair from explicit path");
        load_keypair_from_path(path)
    } else {
        let keypair_path = Path::new(&cli.ledger_path).join("identity.key");
        if keypair_path.exists() {
            // Restart: load previously saved keypair
            info!(path = %keypair_path.display(), "loading existing identity keypair");
            load_keypair_from_path(&keypair_path.to_string_lossy())
        } else {
            // First boot: generate and save
            let keypair = Keypair::generate();
            let mut bytes = Vec::with_capacity(KEYPAIR_SIZE);
            bytes.extend_from_slice(keypair.public_key().as_bytes());
            bytes.extend_from_slice(keypair.secret_key().as_bytes());
            if let Some(parent) = keypair_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match write_keypair_file(&keypair_path, &bytes) {
                Ok(()) => info!(path = %keypair_path.display(), "saved identity keypair"),
                Err(e) => warn!(error = %e, "failed to save identity keypair"),
            }
            Ok(keypair)
        }
    }
}

pub(crate) fn load_keypair_from_path(path: &str) -> Result<Keypair, ValidatorError> {
    let bytes = std::fs::read(path)?;
    if bytes.len() != KEYPAIR_SIZE {
        return Err(ValidatorError::Keypair(format!(
            "invalid keypair file size: expected {KEYPAIR_SIZE}, got {}",
            bytes.len()
        )));
    }
    let split = nusantara_crypto::pubkey::PUBLIC_KEY_BYTES;
    Keypair::from_bytes(&bytes[..split], &bytes[split..])
        .map_err(|e| ValidatorError::Keypair(format!("invalid keypair: {e}")))
}

/// Resolve a hostname to an IP address.
///
/// Blocking DNS is acceptable here — runs once at startup before the async loop.
pub(crate) fn resolve_public_host(host: &str) -> Result<std::net::IpAddr, ValidatorError> {
    // Try parsing as IP first
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Ok(ip);
    }
    // Resolve hostname (e.g. Docker container name)
    let addrs: Vec<_> = std::net::ToSocketAddrs::to_socket_addrs(&(host, 0u16))
        .map_err(|e| {
            ValidatorError::NetworkInit(format!("failed to resolve public host '{host}': {e}"))
        })?
        .collect();
    addrs.first().map(|a| a.ip()).ok_or_else(|| {
        ValidatorError::NetworkInit(format!("public host '{host}' resolved to no addresses"))
    })
}

pub(crate) fn hashv_bank_genesis(genesis_hash: &Hash) -> Hash {
    nusantara_crypto::hashv(&[Hash::zero().as_bytes(), genesis_hash.as_bytes()])
}
