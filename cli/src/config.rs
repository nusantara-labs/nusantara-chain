use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::CliError;

#[derive(Serialize, Deserialize)]
pub struct CliConfig {
    pub rpc_url: String,
    pub keypair_path: String,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self {
            rpc_url: "http://127.0.0.1:8899".to_string(),
            keypair_path: config_dir()
                .join("id.key")
                .to_string_lossy()
                .to_string(),
        }
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("nusantara")
}

fn config_path() -> PathBuf {
    config_dir().join("cli.toml")
}

impl CliConfig {
    pub fn load() -> Result<Self, CliError> {
        let path = config_path();
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            toml::from_str(&content).map_err(|e| CliError::Config(e.to_string()))
        } else {
            Ok(Self::default())
        }
    }

    pub fn save(&self) -> Result<(), CliError> {
        let dir = config_dir();
        std::fs::create_dir_all(&dir)?;
        let content = toml::to_string_pretty(self).map_err(|e| CliError::Config(e.to_string()))?;
        std::fs::write(config_path(), content)?;
        Ok(())
    }
}
