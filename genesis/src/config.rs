use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use nusantara_crypto::{Hash, Keypair, hash};
use serde::Deserialize;

use crate::error::GenesisError;

#[derive(Debug, Deserialize)]
pub struct GenesisConfig {
    pub cluster: ClusterConfig,
    #[serde(default)]
    pub epoch: EpochConfig,
    #[serde(default)]
    pub fees: FeeConfig,
    #[serde(default)]
    pub rent: RentConfig,
    pub validators: Vec<ValidatorConfig>,
    #[serde(default)]
    pub accounts: Vec<AccountConfig>,
    pub faucet: Option<FaucetConfig>,
}

#[derive(Debug, Deserialize)]
pub struct ClusterConfig {
    pub name: String,
    #[serde(default = "default_creation_time")]
    pub creation_time: i64,
}

#[derive(Debug, Deserialize)]
pub struct EpochConfig {
    #[serde(default = "default_slots_per_epoch")]
    pub slots_per_epoch: u64,
}

#[derive(Debug, Deserialize)]
pub struct FeeConfig {
    #[serde(default = "default_lamports_per_signature")]
    pub lamports_per_signature: u64,
}

#[derive(Debug, Deserialize)]
pub struct RentConfig {
    #[serde(default = "default_lamports_per_byte_year")]
    pub lamports_per_byte_year: u64,
    #[serde(default = "default_exemption_threshold")]
    pub exemption_threshold: u64,
    #[serde(default = "default_burn_percent")]
    pub burn_percent: u8,
}

#[derive(Debug, Deserialize)]
pub struct ValidatorConfig {
    pub identity: String,
    #[serde(default = "default_vote_account")]
    pub vote_account: String,
    pub stake_lamports: u64,
    #[serde(default = "default_commission")]
    pub commission: u8,
}

#[derive(Debug, Deserialize)]
pub struct AccountConfig {
    pub address: String,
    pub lamports: u64,
}

#[derive(Debug, Deserialize)]
pub struct FaucetConfig {
    pub address: String,
    pub lamports: u64,
}

// Default value functions for serde
fn default_creation_time() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_secs() as i64
}

fn default_slots_per_epoch() -> u64 {
    432_000
}

fn default_lamports_per_signature() -> u64 {
    5_000
}

fn default_lamports_per_byte_year() -> u64 {
    3_480
}

fn default_exemption_threshold() -> u64 {
    2
}

fn default_burn_percent() -> u8 {
    50
}

fn default_vote_account() -> String {
    "derive".to_string()
}

fn default_commission() -> u8 {
    10
}

impl Default for EpochConfig {
    fn default() -> Self {
        Self {
            slots_per_epoch: default_slots_per_epoch(),
        }
    }
}

impl Default for FeeConfig {
    fn default() -> Self {
        Self {
            lamports_per_signature: default_lamports_per_signature(),
        }
    }
}

impl Default for RentConfig {
    fn default() -> Self {
        Self {
            lamports_per_byte_year: default_lamports_per_byte_year(),
            exemption_threshold: default_exemption_threshold(),
            burn_percent: default_burn_percent(),
        }
    }
}

impl GenesisConfig {
    pub fn load(path: &str) -> Result<Self, GenesisError> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                GenesisError::ConfigNotFound(path.to_string())
            } else {
                GenesisError::ConfigIo(e)
            }
        })?;
        Self::parse(&content)
    }

    pub fn parse(content: &str) -> Result<Self, GenesisError> {
        toml::from_str(content).map_err(|e| GenesisError::ConfigParse(e.to_string()))
    }

    pub fn resolve_address(s: &str, derive_seed: &[u8]) -> Result<Hash, GenesisError> {
        match s {
            "generate" => Ok(Keypair::generate().address()),
            "derive" => Ok(hash(derive_seed)),
            other => Hash::from_base64(other)
                .map_err(|_| GenesisError::InvalidAddress(other.to_string())),
        }
    }
}
