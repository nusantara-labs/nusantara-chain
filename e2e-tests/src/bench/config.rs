use std::time::Duration;

use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(name = "tps-bench", about = "Nusantara TPS benchmark")]
pub struct BenchConfig {
    /// RPC URLs (comma-separated or repeated). Defaults to single-node validator.
    #[arg(long, value_delimiter = ',', default_value = "http://localhost:8899")]
    pub rpc_urls: Vec<String>,

    /// Number of accounts to generate and fund.
    #[arg(long, default_value_t = 100)]
    pub num_accounts: usize,

    /// Total number of transactions to send.
    #[arg(long, default_value_t = 5000)]
    pub tx_count: usize,

    /// Number of concurrent sender tasks (0 = auto-compute based on num_accounts).
    #[arg(long, default_value_t = 0)]
    pub num_senders: usize,

    /// Batch size per sender before yielding.
    #[arg(long, default_value_t = 64)]
    pub batch_size: usize,

    /// Target TPS (0 = unlimited).
    #[arg(long, default_value_t = 0)]
    pub target_tps: u64,

    /// Lamports per transfer transaction.
    #[arg(long, default_value_t = 1000)]
    pub lamports_per_tx: u64,

    /// Lamports to fund each account with (via airdrop).
    #[arg(long, default_value_t = 5_000_000_000)]
    pub fund_amount: u64,

    /// Confirmation timeout in seconds.
    #[arg(long, default_value_t = 120)]
    pub confirm_timeout_secs: u64,

    /// Batch size for parallel funding (accounts per chunk).
    #[arg(long, default_value_t = 256)]
    pub funding_batch_size: usize,

    /// Max concurrent airdrop requests during funding.
    #[arg(long, default_value_t = 32)]
    pub funding_concurrency: usize,

    /// Output format: human or json.
    #[arg(long, default_value = "human")]
    pub output: OutputFormat,
}

impl BenchConfig {
    pub fn confirm_timeout(&self) -> Duration {
        Duration::from_secs(self.confirm_timeout_secs)
    }

    /// Compute effective number of senders.
    /// When `num_senders == 0`, auto-scales based on `num_accounts`.
    pub fn effective_num_senders(&self) -> usize {
        if self.num_senders == 0 {
            (self.num_accounts / 10).clamp(4, 64)
        } else {
            self.num_senders
        }
    }
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum OutputFormat {
    Human,
    Json,
}
