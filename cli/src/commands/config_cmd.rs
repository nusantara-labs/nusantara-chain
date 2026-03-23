use clap::Subcommand;

use crate::config::CliConfig;
use crate::error::CliError;

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Show current configuration
    Get,
    /// Set configuration values
    Set {
        /// RPC URL
        #[arg(long)]
        url: Option<String>,
        /// Keypair path
        #[arg(long)]
        keypair: Option<String>,
    },
}

pub fn run(action: ConfigAction) -> Result<(), CliError> {
    match action {
        ConfigAction::Get => {
            let config = CliConfig::load()?;
            println!("RPC URL:      {}", config.rpc_url);
            println!("Keypair path: {}", config.keypair_path);
        }
        ConfigAction::Set { url, keypair } => {
            let mut config = CliConfig::load()?;
            if let Some(u) = url {
                config.rpc_url = u;
            }
            if let Some(k) = keypair {
                config.keypair_path = k;
            }
            config.save()?;
            println!("Config updated:");
            println!("  RPC URL:      {}", config.rpc_url);
            println!("  Keypair path: {}", config.keypair_path);
        }
    }
    Ok(())
}
