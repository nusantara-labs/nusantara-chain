use clap::Parser;

#[derive(Parser)]
#[command(name = "nusantara-validator", about = "Nusantara blockchain validator")]
pub struct Cli {
    #[arg(long, default_value = "ledger")]
    pub ledger_path: String,

    #[arg(long)]
    pub genesis_config: Option<String>,

    #[arg(long)]
    pub identity: Option<String>,

    #[arg(long, default_value = "info")]
    pub log_level: String,

    #[arg(long, default_value = "127.0.0.1:9090")]
    pub metrics_addr: String,

    /// Gossip protocol bind address
    #[arg(long, default_value = "0.0.0.0:8000")]
    pub gossip_addr: String,

    /// Turbine (block propagation) bind address
    #[arg(long, default_value = "0.0.0.0:8001")]
    pub turbine_addr: String,

    /// Repair service bind address
    #[arg(long, default_value = "0.0.0.0:8002")]
    pub repair_addr: String,

    /// TPU (transaction processing unit) bind address
    #[arg(long, default_value = "0.0.0.0:8003")]
    pub tpu_addr: String,

    /// TPU forward bind address
    #[arg(long, default_value = "0.0.0.0:8004")]
    pub tpu_forward_addr: String,

    /// Entrypoint addresses for peer discovery (gossip endpoints)
    #[arg(long)]
    pub entrypoints: Vec<String>,

    /// Shred version for network compatibility
    #[arg(long, default_value = "1")]
    pub shred_version: u16,

    /// RPC server bind address
    #[arg(long, default_value = "0.0.0.0:8899")]
    pub rpc_addr: String,

    /// Enable the faucet (airdrop) endpoint using the validator identity keypair
    #[arg(long)]
    pub enable_faucet: bool,

    /// Timeout in ms to wait for a block from the leader before considering the slot skipped
    #[arg(long, default_value = "800")]
    pub leader_timeout_ms: u64,

    /// Interval in slots between automatic snapshots (0 = disabled)
    #[arg(long, default_value = "0")]
    pub snapshot_interval: u64,

    /// Initialize genesis and exit without running the validator
    #[arg(long)]
    pub init_only: bool,

    /// Public hostname or IP to advertise to peers (resolves to IP at startup).
    /// Use this in Docker/Kubernetes where bind address (0.0.0.0) differs from
    /// the externally reachable address. Example: --public-host=validator-1
    #[arg(long)]
    pub public_host: Option<String>,

    /// Extra validator keypair files for multi-validator genesis.
    /// Comma-separated paths. Each maps to a "generate" identity in genesis.toml
    /// in order (first is validator 2, second is validator 3, etc.).
    #[arg(long, value_delimiter = ',')]
    pub extra_validator_keys: Vec<String>,

    /// Generate a keypair and save to the given path, then exit.
    #[arg(long)]
    pub generate_keypair: Option<String>,

    /// Override PoH hashes per tick (default: compiled 12500, use 1 for benchmarks)
    #[arg(long)]
    pub hashes_per_tick: Option<u64>,

    /// Maximum number of ledger slots to retain (older slots are pruned).
    /// Set to 0 to disable pruning.
    #[arg(long, default_value = "256")]
    pub max_ledger_slots: u64,

    /// Path to TLS certificate for HTTPS RPC
    #[arg(long)]
    pub rpc_tls_cert: Option<String>,

    /// Path to TLS private key for HTTPS RPC
    #[arg(long)]
    pub rpc_tls_key: Option<String>,

    /// Maximum number of transactions to include per slot (default: 65536)
    #[arg(long, default_value = "65536")]
    pub max_txs_per_slot: usize,
}
