pub mod error;
pub mod config;
pub mod builder;

pub use error::GenesisError;
pub use config::GenesisConfig;
pub use builder::{
    GenesisBuilder, GenesisResult, GenesisValidatorInfo,
    FAUCET_KEYPAIR_KEY, GENESIS_HASH_KEY, VALIDATORS_KEY,
};

use nusantara_crypto::Keypair;
use nusantara_crypto::keypair::SECRET_KEY_BYTES;
use nusantara_crypto::pubkey::PUBLIC_KEY_BYTES;
use nusantara_storage::Storage;
use nusantara_storage::cf::CF_DEFAULT;

/// Load the faucet keypair that was persisted during genesis.
/// Returns `None` if no faucet keypair was stored (e.g. a literal address was used).
pub fn load_faucet_keypair(storage: &Storage) -> Option<Keypair> {
    let bytes = storage.get_cf(CF_DEFAULT, FAUCET_KEYPAIR_KEY).ok()??;
    if bytes.len() != PUBLIC_KEY_BYTES + SECRET_KEY_BYTES {
        return None;
    }
    Keypair::from_bytes(&bytes[..PUBLIC_KEY_BYTES], &bytes[PUBLIC_KEY_BYTES..]).ok()
}

#[cfg(test)]
mod tests {
    use nusantara_storage::Storage;
    use nusantara_storage::cf::CF_DEFAULT;
    use tempfile::TempDir;

    use super::*;

    fn test_storage() -> (Storage, TempDir) {
        let dir = TempDir::new().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        (storage, dir)
    }

    fn minimal_config() -> &'static str {
        r#"
[cluster]
name = "test-cluster"
creation_time = 1000

[[validators]]
identity = "generate"
vote_account = "derive"
stake_lamports = 1_000_000_000
commission = 10
"#
    }

    fn full_config() -> &'static str {
        r#"
[cluster]
name = "full-test-cluster"
creation_time = 2000

[epoch]
slots_per_epoch = 100

[fees]
lamports_per_signature = 10000

[rent]
lamports_per_byte_year = 3480
exemption_threshold = 2
burn_percent = 50

[[validators]]
identity = "generate"
vote_account = "derive"
stake_lamports = 500_000_000_000
commission = 5

[[validators]]
identity = "generate"
vote_account = "derive"
stake_lamports = 300_000_000_000
commission = 15

[[accounts]]
address = "generate"
lamports = 1_000_000_000

[faucet]
address = "generate"
lamports = 1_000_000_000_000_000
"#
    }

    #[test]
    fn config_parse_minimal() {
        let config = GenesisConfig::parse(minimal_config()).unwrap();
        assert_eq!(config.cluster.name, "test-cluster");
        assert_eq!(config.cluster.creation_time, 1000);
        assert_eq!(config.validators.len(), 1);
        assert_eq!(config.validators[0].stake_lamports, 1_000_000_000);
        // Defaults
        assert_eq!(config.epoch.slots_per_epoch, 432_000);
        assert_eq!(config.fees.lamports_per_signature, 5_000);
        assert!(config.faucet.is_none());
        assert!(config.accounts.is_empty());
    }

    #[test]
    fn config_parse_full() {
        let config = GenesisConfig::parse(full_config()).unwrap();
        assert_eq!(config.cluster.name, "full-test-cluster");
        assert_eq!(config.epoch.slots_per_epoch, 100);
        assert_eq!(config.fees.lamports_per_signature, 10000);
        assert_eq!(config.validators.len(), 2);
        assert_eq!(config.validators[0].commission, 5);
        assert_eq!(config.validators[1].stake_lamports, 300_000_000_000);
        assert_eq!(config.accounts.len(), 1);
        assert!(config.faucet.is_some());
        assert_eq!(config.faucet.unwrap().lamports, 1_000_000_000_000_000);
    }

    #[test]
    fn config_parse_defaults() {
        let config = GenesisConfig::parse(minimal_config()).unwrap();
        assert_eq!(config.rent.lamports_per_byte_year, 3_480);
        assert_eq!(config.rent.exemption_threshold, 2);
        assert_eq!(config.rent.burn_percent, 50);
        assert_eq!(config.validators[0].vote_account, "derive");
    }

    #[test]
    fn build_genesis_creates_accounts() {
        let (storage, _dir) = test_storage();
        let config_str = r#"
[cluster]
name = "test"
creation_time = 1000

[[validators]]
identity = "generate"
stake_lamports = 1_000_000_000

[faucet]
address = "generate"
lamports = 5_000_000_000
"#;
        let config = GenesisConfig::parse(config_str).unwrap();
        let builder = GenesisBuilder::new(&config, &storage);
        let result = builder.build().unwrap();

        assert_eq!(result.cluster_name, "test");
        assert_eq!(result.validator_count, 1);
        assert!(result.total_supply > 5_000_000_000);
    }

    #[test]
    fn build_genesis_creates_validators() {
        let (storage, _dir) = test_storage();
        let config = GenesisConfig::parse(minimal_config()).unwrap();
        let builder = GenesisBuilder::new(&config, &storage);
        let result = builder.build().unwrap();

        assert_eq!(result.validator_count, 1);
        assert_eq!(result.total_stake, 1_000_000_000);

        // Verify validator info was stored
        let data = storage.get_cf(CF_DEFAULT, VALIDATORS_KEY).unwrap().unwrap();
        let infos: Vec<GenesisValidatorInfo> = borsh::from_slice(&data).unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].stake_lamports, 1_000_000_000);
    }

    #[test]
    fn build_genesis_writes_sysvars() {
        use nusantara_sysvar_program::{Clock, EpochScheduleSysvar, RentSysvar};

        let (storage, _dir) = test_storage();
        let config = GenesisConfig::parse(minimal_config()).unwrap();
        let builder = GenesisBuilder::new(&config, &storage);
        builder.build().unwrap();

        let clock: Clock = storage.get_sysvar::<Clock>().unwrap().unwrap();
        assert_eq!(clock.slot, 0);
        assert_eq!(clock.epoch, 0);
        assert_eq!(clock.unix_timestamp, 1000);

        let rent: RentSysvar = storage.get_sysvar::<RentSysvar>().unwrap().unwrap();
        assert_eq!(rent.0.lamports_per_byte_year, 3480);

        let epoch: EpochScheduleSysvar = storage
            .get_sysvar::<EpochScheduleSysvar>()
            .unwrap()
            .unwrap();
        assert_eq!(epoch.0.slots_per_epoch, 432_000);
    }

    #[test]
    fn build_genesis_idempotent() {
        let (storage, _dir) = test_storage();
        let config = GenesisConfig::parse(minimal_config()).unwrap();

        let builder = GenesisBuilder::new(&config, &storage);
        builder.build().unwrap();

        // Second build should fail
        let builder2 = GenesisBuilder::new(&config, &storage);
        let err = builder2.build().unwrap_err();
        assert!(matches!(err, GenesisError::AlreadyInitialized(_)));
    }

    #[test]
    fn build_genesis_no_validators_error() {
        let config_str = r#"
[cluster]
name = "test"
creation_time = 1000
"#;
        // TOML parsing fails because validators is a required field
        let result = GenesisConfig::parse(config_str);
        assert!(result.is_err());
    }
}
