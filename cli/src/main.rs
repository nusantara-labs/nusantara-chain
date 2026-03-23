mod error;
mod config;
mod client;
mod output;
mod keypair;
mod commands;

use clap::Parser;

use crate::commands::Commands;
use crate::config::CliConfig;
use crate::error::CliError;

#[derive(Parser)]
#[command(name = "nusantara", about = "Nusantara blockchain CLI")]
pub struct Cli {
    /// RPC URL (overrides config)
    #[arg(long, short = 'u', global = true)]
    pub url: Option<String>,

    /// Path to keypair file (overrides config)
    #[arg(long, short = 'k', global = true)]
    pub keypair: Option<String>,

    /// Output format: text or json
    #[arg(long, short = 'o', global = true, default_value = "text")]
    pub output: String,

    #[command(subcommand)]
    pub command: Commands,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    if let Err(e) = run(cli).await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), CliError> {
    let config = CliConfig::load()?;
    let url = cli.url.as_deref().unwrap_or(&config.rpc_url);
    let keypair_path = cli.keypair.as_deref().unwrap_or(&config.keypair_path);
    let json_output = cli.output == "json";

    commands::dispatch(cli.command, url, keypair_path, json_output).await
}
