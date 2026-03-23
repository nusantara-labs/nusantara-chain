mod block_producer;
mod block_replayer;
mod bootstrap;
mod cli;
mod constants;
mod epoch;
mod error;
mod fork_manager;
mod helpers;
mod identity;
mod node;
mod replay;
mod services;
mod slot_clock;
mod slot_loop;
mod snapshot_fetcher;
mod vote_tx;
mod voting;

use std::net::SocketAddr;

use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::cli::Cli;
use crate::node::ValidatorNode;

#[tokio::main]
async fn main() {
    // Install rustls CryptoProvider before any TLS/QUIC usage
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls CryptoProvider");

    let cli = Cli::parse();

    // --generate-keypair: create a keypair file and exit (no tracing needed)
    if let Some(ref path) = cli.generate_keypair {
        let kp = nusantara_crypto::Keypair::generate();
        let mut bytes = Vec::with_capacity(64);
        bytes.extend_from_slice(kp.public_key().as_bytes());
        bytes.extend_from_slice(kp.secret_key().as_bytes());
        std::fs::write(path, &bytes).expect("failed to write keypair file");
        println!("Keypair generated: {path}");
        println!("Identity: {}", kp.address().to_base64());
        return;
    }

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(&cli.log_level)),
        )
        .init();

    // Initialize metrics exporter
    let addr: SocketAddr = cli
        .metrics_addr
        .parse()
        .expect("invalid metrics address");
    metrics_exporter_prometheus::PrometheusBuilder::new()
        .with_http_listener(addr)
        .install()
        .expect("failed to install metrics exporter");
    info!(addr = %cli.metrics_addr, "metrics exporter started");

    // Pre-boot: attempt to fetch a snapshot from entrypoints for fast bootstrap.
    // This runs before boot() so that the snapshot file is already on disk when
    // boot() reaches its snapshot restore step (2b).
    if !cli.entrypoints.is_empty() && !cli.init_only {
        let snapshot_dir = std::path::Path::new(&cli.ledger_path).join("snapshots");
        match snapshot_fetcher::fetch_snapshot_from_entrypoints(&cli.entrypoints, &snapshot_dir)
            .await
        {
            Ok(Some(path)) => {
                info!(path = %path.display(), "snapshot fetched from entrypoint");
            }
            Ok(None) => {
                info!("no snapshot available from entrypoints, continuing with genesis");
            }
            Err(e) => {
                tracing::warn!(error = %e, "snapshot fetch failed, continuing without snapshot");
            }
        }
    }

    // Boot validator
    let mut node = match ValidatorNode::boot(&cli) {
        Ok(node) => node,
        Err(e) => {
            tracing::error!("Failed to boot validator: {e}");
            std::process::exit(1);
        }
    };

    // --init-only: flush storage to disk and exit
    if cli.init_only {
        if let Err(e) = node.flush_storage() {
            tracing::error!("Failed to flush storage: {e}");
            std::process::exit(1);
        }
        info!("genesis initialized, exiting (--init-only)");
        return;
    }

    // Run — shutdown is handled inside run() via watch channel + ctrl_c
    if let Err(e) = node.run(&cli).await {
        tracing::error!("Validator error: {e}");
        std::process::exit(1);
    }

    info!("validator shutdown complete");
}
