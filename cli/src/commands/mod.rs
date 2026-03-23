pub mod keygen;
pub mod balance;
pub mod transfer;
pub mod airdrop;
pub mod account;
pub mod block;
pub mod slot;
pub mod transaction;
pub mod epoch;
pub mod leader;
pub mod validators;
pub mod stake;
pub mod vote;
pub mod config_cmd;
pub mod program;

use clap::Subcommand;

use crate::error::CliError;

#[derive(Subcommand)]
pub enum Commands {
    /// Generate a new keypair
    Keygen {
        /// Output file path
        #[arg(long, short = 'o')]
        outfile: Option<String>,
    },

    /// Check account balance
    Balance {
        /// Account address (default: own keypair)
        address: Option<String>,
    },

    /// Transfer NUSA
    Transfer {
        /// Recipient address
        to: String,
        /// Amount in NUSA
        amount: f64,
    },

    /// Request testnet airdrop
    Airdrop {
        /// Amount in NUSA
        amount: f64,
        /// Recipient address (default: own keypair)
        #[arg(long)]
        recipient: Option<String>,
    },

    /// View account info
    Account {
        /// Account address
        address: String,
    },

    /// View block
    Block {
        /// Slot number
        slot: u64,
    },

    /// Current slot info
    Slot,

    /// View transaction status
    Transaction {
        /// Transaction hash
        hash: String,
    },

    /// Current epoch info
    EpochInfo,

    /// Leader schedule
    LeaderSchedule {
        /// Epoch number (default: current)
        epoch: Option<u64>,
    },

    /// List validators
    Validators,

    // ── Stake operations ──

    /// Create a new stake account
    CreateStakeAccount {
        /// Path to stake account keypair
        stake_keypair: String,
        /// Amount in NUSA to fund
        amount: f64,
    },

    /// Delegate stake to a vote account
    DelegateStake {
        /// Stake account address
        stake_account: String,
        /// Vote account address
        vote_account: String,
    },

    /// Deactivate stake
    DeactivateStake {
        /// Stake account address
        stake_account: String,
    },

    /// Withdraw from stake account
    WithdrawStake {
        /// Stake account address
        stake_account: String,
        /// Recipient address
        to: String,
        /// Amount in NUSA
        amount: f64,
    },

    /// View stake account
    StakeAccount {
        /// Stake account address
        address: String,
    },

    // ── Vote operations ──

    /// Create a new vote account
    CreateVoteAccount {
        /// Path to vote account keypair
        vote_keypair: String,
        /// Identity (node) address
        identity: String,
        /// Commission percentage (0-100)
        commission: u8,
    },

    /// View vote account
    VoteAccount {
        /// Vote account address
        address: String,
    },

    /// Authorize a new voter or withdrawer
    VoteAuthorize {
        /// Vote account address
        vote_account: String,
        /// New authorized address
        new_auth: String,
        /// Type: "voter" or "withdrawer"
        auth_type: String,
    },

    /// Update vote account commission
    VoteUpdateCommission {
        /// Vote account address
        vote_account: String,
        /// New commission percentage (0-100)
        commission: u8,
    },

    // ── Program operations ──

    /// Deploy a WASM program
    ProgramDeploy {
        /// Path to WASM file
        wasm_file: String,
    },

    /// Show program info
    ProgramShow {
        /// Program address
        address: String,
    },

    /// Upgrade a deployed program
    ProgramUpgrade {
        /// Program address
        program_address: String,
        /// Path to new WASM file
        wasm_file: String,
    },

    // ── Config ──

    /// CLI configuration
    Config {
        #[command(subcommand)]
        action: config_cmd::ConfigAction,
    },
}

pub async fn dispatch(
    command: Commands,
    url: &str,
    keypair_path: &str,
    json: bool,
) -> Result<(), CliError> {
    match command {
        Commands::Keygen { outfile } => keygen::run(outfile),
        Commands::Balance { address } => balance::run(url, keypair_path, address, json).await,
        Commands::Transfer { to, amount } => {
            transfer::run(url, keypair_path, &to, amount, json).await
        }
        Commands::Airdrop { amount, recipient } => {
            airdrop::run(url, keypair_path, amount, recipient, json).await
        }
        Commands::Account { address } => account::run(url, &address, json).await,
        Commands::Block { slot } => block::run(url, slot, json).await,
        Commands::Slot => slot::run(url, json).await,
        Commands::Transaction { hash } => transaction::run(url, &hash, json).await,
        Commands::EpochInfo => epoch::run(url, json).await,
        Commands::LeaderSchedule { epoch } => leader::run(url, epoch, json).await,
        Commands::Validators => validators::run(url, json).await,

        Commands::CreateStakeAccount {
            stake_keypair,
            amount,
        } => stake::create(url, keypair_path, &stake_keypair, amount, json).await,
        Commands::DelegateStake {
            stake_account,
            vote_account,
        } => stake::delegate(url, keypair_path, &stake_account, &vote_account, json).await,
        Commands::DeactivateStake { stake_account } => {
            stake::deactivate(url, keypair_path, &stake_account, json).await
        }
        Commands::WithdrawStake {
            stake_account,
            to,
            amount,
        } => stake::withdraw(url, keypair_path, &stake_account, &to, amount, json).await,
        Commands::StakeAccount { address } => stake::view(url, &address, json).await,

        Commands::CreateVoteAccount {
            vote_keypair,
            identity,
            commission,
        } => vote::create(url, keypair_path, &vote_keypair, &identity, commission, json).await,
        Commands::VoteAccount { address } => vote::view(url, &address, json).await,
        Commands::VoteAuthorize {
            vote_account,
            new_auth,
            auth_type,
        } => {
            vote::authorize(url, keypair_path, &vote_account, &new_auth, &auth_type, json).await
        }
        Commands::VoteUpdateCommission {
            vote_account,
            commission,
        } => vote::update_commission(url, keypair_path, &vote_account, commission, json).await,

        Commands::ProgramDeploy { wasm_file } => {
            program::deploy(url, keypair_path, &wasm_file, json).await
        }
        Commands::ProgramShow { address } => program::show(url, &address, json).await,
        Commands::ProgramUpgrade {
            program_address,
            wasm_file,
        } => program::upgrade(url, keypair_path, &program_address, &wasm_file, json).await,

        Commands::Config { action } => config_cmd::run(action),
    }
}
