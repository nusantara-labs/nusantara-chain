use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

use axum::Router;
use dashmap::DashMap;
use nusantara_consensus::bank::ConsensusBank;
use nusantara_consensus::leader_schedule::{LeaderSchedule, LeaderScheduleGenerator};
use nusantara_core::epoch::EpochSchedule;
use nusantara_core::Transaction;
use nusantara_crypto::{Hash, Keypair};
use nusantara_gossip::cluster_info::ClusterInfo;
use nusantara_mempool::Mempool;
use nusantara_storage::Storage;
use serde::Serialize;
use tokio::sync::{Semaphore, broadcast, mpsc, watch};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::info;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::handlers;
use crate::rate_limiter::{RpcRateLimitLayer, RpcRateLimiter};
use crate::types;

/// Default broadcast channel capacity for pubsub events.
/// Sized to absorb short bursts without dropping events for connected clients.
const PUBSUB_CHANNEL_CAPACITY: usize = 4096;

/// Maximum concurrent WebSocket connections.
pub const MAX_WS_CONNECTIONS: usize = 1000;

/// Maximum subscriptions per WebSocket connection (slot + block + N signatures).
pub const MAX_SUBSCRIPTIONS_PER_CONN: usize = 10_000;

/// Faucet cooldown per recipient address (seconds).
pub const FAUCET_COOLDOWN_PER_ADDRESS_SECS: u64 = 60;

/// Faucet cooldown per source IP (seconds).
pub const FAUCET_COOLDOWN_PER_IP_SECS: u64 = 10;

pub type SharedLeaderCache = Arc<parking_lot::RwLock<HashMap<u64, LeaderSchedule>>>;

/// Events published to WebSocket subscribers via a broadcast channel.
///
/// Each variant is tagged with `"type"` so clients can filter on the JSON `type` field.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum PubsubEvent {
    SlotUpdate {
        slot: u64,
        parent: u64,
        root: u64,
    },
    BlockNotification {
        slot: u64,
        block_hash: String,
        tx_count: u64,
    },
    SignatureNotification {
        signature: String,
        slot: u64,
        status: String,
    },
}

#[derive(Clone)]
pub struct RpcState {
    pub storage: Arc<Storage>,
    pub bank: Arc<ConsensusBank>,
    pub mempool: Arc<Mempool>,
    pub leader_cache: SharedLeaderCache,
    pub leader_schedule_generator: LeaderScheduleGenerator,
    pub epoch_schedule: EpochSchedule,
    pub genesis_hash: Hash,
    pub faucet_keypair: Option<Arc<Keypair>>,
    pub identity: Hash,
    pub cluster_info: Arc<ClusterInfo>,
    pub consecutive_skips: Arc<AtomicU64>,
    /// Forward transactions to the TPU forwarder for leader routing.
    /// When set, RPC handlers send a copy of each submitted transaction
    /// through this channel so the TPU layer can route it to the current leader.
    pub tx_forward_sender: Option<mpsc::Sender<Transaction>>,
    /// Broadcast sender for real-time pubsub events delivered over WebSocket.
    pub pubsub_tx: broadcast::Sender<PubsubEvent>,
    /// Directory where snapshot files are stored (e.g. `{ledger}/snapshots/`).
    pub snapshot_dir: PathBuf,
    /// Semaphore that bounds the number of concurrent WebSocket connections.
    pub ws_semaphore: Arc<Semaphore>,
    /// Per-address cooldown tracker for faucet requests.
    /// Key: recipient address (base64 string), Value: last airdrop timestamp.
    pub faucet_address_cooldowns: DashMap<String, Instant>,
    /// Per-IP cooldown tracker for faucet requests.
    /// Key: source IP, Value: last airdrop timestamp.
    pub faucet_ip_cooldowns: DashMap<IpAddr, Instant>,
}

impl RpcState {
    /// Create a new broadcast channel pair for pubsub events.
    /// Returns the `Sender` half that should be stored in `RpcState` and
    /// also used by the validator to publish events.
    pub fn new_pubsub_channel() -> broadcast::Sender<PubsubEvent> {
        let (tx, _rx) = broadcast::channel(PUBSUB_CHANNEL_CAPACITY);
        tx
    }

    /// Create a new WebSocket connection semaphore with the default limit.
    pub fn new_ws_semaphore() -> Arc<Semaphore> {
        Arc::new(Semaphore::new(MAX_WS_CONNECTIONS))
    }

    /// Check the faucet per-address cooldown. Returns `Ok(())` if the address
    /// has not been airdropped within the cooldown window, or an `RpcError` if
    /// it has.
    pub fn check_faucet_address_cooldown(&self, address: &str) -> Result<(), crate::RpcError> {
        if let Some(last) = self.faucet_address_cooldowns.get(address) {
            let elapsed = last.elapsed().as_secs();
            if elapsed < FAUCET_COOLDOWN_PER_ADDRESS_SECS {
                let remaining = FAUCET_COOLDOWN_PER_ADDRESS_SECS - elapsed;
                return Err(crate::RpcError::RateLimited(format!(
                    "address cooldown: retry in {remaining}s"
                )));
            }
        }
        Ok(())
    }

    /// Check the faucet per-IP cooldown. Returns `Ok(())` if the IP has not
    /// made a faucet request within the cooldown window.
    pub fn check_faucet_ip_cooldown(&self, ip: IpAddr) -> Result<(), crate::RpcError> {
        // Exempt localhost and Docker bridge networks
        if crate::rate_limiter::is_local_or_docker(ip) {
            return Ok(());
        }
        if let Some(last) = self.faucet_ip_cooldowns.get(&ip) {
            let elapsed = last.elapsed().as_secs();
            if elapsed < FAUCET_COOLDOWN_PER_IP_SECS {
                let remaining = FAUCET_COOLDOWN_PER_IP_SECS - elapsed;
                return Err(crate::RpcError::RateLimited(format!(
                    "IP cooldown: retry in {remaining}s"
                )));
            }
        }
        Ok(())
    }

    /// Record a successful faucet airdrop for both address and IP cooldowns.
    pub fn record_faucet_airdrop(&self, address: &str, ip: IpAddr) {
        // Don't record cooldowns for local/Docker IPs
        if crate::rate_limiter::is_local_or_docker(ip) {
            return;
        }
        let now = Instant::now();
        self.faucet_address_cooldowns
            .insert(address.to_string(), now);
        self.faucet_ip_cooldowns.insert(ip, now);
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(
        handlers::health::health,
        handlers::slot::get_slot,
        handlers::slot::get_blockhash,
        handlers::account::get_account,
        handlers::block::get_block,
        handlers::block::get_block_transactions,
        handlers::transaction::get_transaction,
        handlers::transaction::send_transaction,
        handlers::transaction::send_and_confirm,
        handlers::epoch::get_epoch_info,
        handlers::leader::get_leader_schedule,
        handlers::leader::get_leader_schedule_epoch,
        handlers::validator::get_validators,
        handlers::stake::get_stake_account,
        handlers::vote::get_vote_account,
        handlers::signatures::get_signatures,
        handlers::faucet::airdrop,
        handlers::faucet::airdrop_and_confirm,
        handlers::snapshot::get_latest_snapshot,
        handlers::snapshot_download::download_snapshot,
        handlers::program::get_program,
        handlers::accounts_by::get_accounts_by_owner,
        handlers::accounts_by::get_accounts_by_program,
        handlers::proof::get_account_proof,
    ),
    components(schemas(
        types::HealthResponse,
        types::AccountResponse,
        types::BlockResponse,
        types::BlockTransactionEntry,
        types::BlockTransactionsResponse,
        types::TransactionStatusResponse,
        types::SendTransactionRequest,
        types::SendTransactionResponse,
        types::SendAndConfirmRequest,
        types::SendAndConfirmResponse,
        types::SlotResponse,
        types::BlockhashResponse,
        types::EpochInfoResponse,
        types::LeaderScheduleResponse,
        types::LeaderSlotEntry,
        types::ValidatorsResponse,
        types::ValidatorEntry,
        types::StakeAccountResponse,
        types::VoteAccountResponse,
        types::EpochCreditEntry,
        types::SignaturesResponse,
        types::SignatureEntry,
        types::AirdropRequest,
        types::AirdropResponse,
        types::AirdropAndConfirmRequest,
        types::AirdropAndConfirmResponse,
        handlers::snapshot::SnapshotResponse,
        types::ProgramResponse,
        handlers::accounts_by::AccountsByResponse,
        handlers::accounts_by::AccountsByEntry,
        handlers::proof::AccountProofResponse,
        handlers::proof::ProofData,
    ))
)]
struct ApiDoc;

/// TLS configuration for HTTPS RPC.
///
/// When provided to `RpcServer::serve_tls`, the server will accept TLS
/// connections using the certificate chain and private key loaded from the
/// specified PEM files.
pub struct RpcTlsConfig {
    acceptor: tokio_rustls::TlsAcceptor,
}

impl RpcTlsConfig {
    /// Build a TLS configuration from PEM-encoded certificate and key files.
    ///
    /// The cert file may contain a full chain (leaf + intermediates).
    /// The key file must contain a single PKCS#8 or RSA private key.
    pub fn from_pem_files(cert_path: &Path, key_path: &Path) -> Result<Self, crate::RpcError> {
        use rustls::pki_types::PrivateKeyDer;

        let cert_bytes = std::fs::read(cert_path).map_err(|e| {
            crate::RpcError::Internal(format!(
                "failed to read TLS cert {}: {e}",
                cert_path.display()
            ))
        })?;
        let key_bytes = std::fs::read(key_path).map_err(|e| {
            crate::RpcError::Internal(format!(
                "failed to read TLS key {}: {e}",
                key_path.display()
            ))
        })?;

        let certs: Vec<_> = rustls_pemfile::certs(&mut cert_bytes.as_slice())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| crate::RpcError::Internal(format!("invalid TLS cert PEM: {e}")))?;

        if certs.is_empty() {
            return Err(crate::RpcError::Internal(
                "TLS cert file contains no certificates".to_string(),
            ));
        }

        let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key_bytes.as_slice())
            .map_err(|e| crate::RpcError::Internal(format!("invalid TLS key PEM: {e}")))?
            .ok_or_else(|| {
                crate::RpcError::Internal("TLS key file contains no private key".to_string())
            })?;

        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| crate::RpcError::Internal(format!("TLS config error: {e}")))?;

        Ok(Self {
            acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(config)),
        })
    }
}

pub struct RpcServer;

impl RpcServer {
    pub fn router(state: Arc<RpcState>) -> Router {
        let rate_limiter = RpcRateLimiter::new();

        let api_routes = Router::new()
            .route("/v1/health", axum::routing::get(handlers::health::health))
            .route("/v1/slot", axum::routing::get(handlers::slot::get_slot))
            .route(
                "/v1/blockhash",
                axum::routing::get(handlers::slot::get_blockhash),
            )
            .route(
                "/v1/account/{address}",
                axum::routing::get(handlers::account::get_account),
            )
            .route(
                "/v1/block/{slot}",
                axum::routing::get(handlers::block::get_block),
            )
            .route(
                "/v1/block/{slot}/transactions",
                axum::routing::get(handlers::block::get_block_transactions),
            )
            .route(
                "/v1/transaction/{hash}",
                axum::routing::get(handlers::transaction::get_transaction),
            )
            .route(
                "/v1/transaction/send",
                axum::routing::post(handlers::transaction::send_transaction),
            )
            .route(
                "/v1/transaction/send-and-confirm",
                axum::routing::post(handlers::transaction::send_and_confirm),
            )
            .route(
                "/v1/epoch-info",
                axum::routing::get(handlers::epoch::get_epoch_info),
            )
            .route(
                "/v1/leader-schedule",
                axum::routing::get(handlers::leader::get_leader_schedule),
            )
            .route(
                "/v1/leader-schedule/{epoch}",
                axum::routing::get(handlers::leader::get_leader_schedule_epoch),
            )
            .route(
                "/v1/validators",
                axum::routing::get(handlers::validator::get_validators),
            )
            .route(
                "/v1/stake-account/{address}",
                axum::routing::get(handlers::stake::get_stake_account),
            )
            .route(
                "/v1/vote-account/{address}",
                axum::routing::get(handlers::vote::get_vote_account),
            )
            .route(
                "/v1/signatures/{address}",
                axum::routing::get(handlers::signatures::get_signatures),
            )
            .route(
                "/v1/airdrop",
                axum::routing::post(handlers::faucet::airdrop),
            )
            .route(
                "/v1/airdrop-and-confirm",
                axum::routing::post(handlers::faucet::airdrop_and_confirm),
            )
            .route(
                "/v1/snapshot/latest",
                axum::routing::get(handlers::snapshot::get_latest_snapshot),
            )
            .route(
                "/v1/snapshot/download",
                axum::routing::get(handlers::snapshot_download::download_snapshot),
            )
            .route(
                "/v1/program/{address}",
                axum::routing::get(handlers::program::get_program),
            )
            .route(
                "/v1/account/{address}/proof",
                axum::routing::get(handlers::proof::get_account_proof),
            )
            .route(
                "/v1/accounts/by-owner/{owner}",
                axum::routing::get(handlers::accounts_by::get_accounts_by_owner),
            )
            .route(
                "/v1/accounts/by-program/{program}",
                axum::routing::get(handlers::accounts_by::get_accounts_by_program),
            )
            .route("/v1/ws", axum::routing::get(handlers::ws::ws_handler))
            .route(
                "/rpc",
                axum::routing::post(handlers::jsonrpc_dispatch::handle_jsonrpc),
            );

        Router::new()
            .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
            .merge(api_routes)
            .layer(RpcRateLimitLayer::new(rate_limiter))
            .layer(CorsLayer::permissive())
            .layer(TraceLayer::new_for_http())
            .with_state(state)
    }

    pub async fn serve(
        addr: SocketAddr,
        state: Arc<RpcState>,
        tls: Option<RpcTlsConfig>,
        shutdown: watch::Receiver<bool>,
    ) {
        if let Some(tls_config) = tls {
            Self::serve_tls(addr, state, tls_config, shutdown).await;
        } else {
            Self::serve_plain(addr, state, shutdown).await;
        }
    }

    /// Serve plain HTTP (no TLS).
    ///
    /// Uses `into_make_service_with_connect_info` so that `ConnectInfo<SocketAddr>`
    /// is available to the rate-limiting middleware and handlers that need the
    /// client's IP address.
    async fn serve_plain(
        addr: SocketAddr,
        state: Arc<RpcState>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let app = Self::router(state);

        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(error = %e, "failed to bind RPC server");
                return;
            }
        };

        info!(addr = %addr, "RPC server listening (HTTP)");
        metrics::counter!("nusantara_rpc_server_started").increment(1);

        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = shutdown.wait_for(|v| *v).await;
            info!("RPC server shutting down");
        })
        .await
        .unwrap_or_else(|e| tracing::error!(error = %e, "RPC server error"));
    }

    /// Serve HTTPS with TLS termination.
    ///
    /// Accepts TLS connections using `tokio_rustls::TlsAcceptor`, then
    /// hands the decrypted stream to axum via `hyper`. Each accepted
    /// connection is spawned as an independent task for concurrency.
    ///
    /// Note: `ConnectInfo` is injected manually as a request extension from
    /// the `remote_addr` captured at accept time so the rate limiter still
    /// has access to the client IP.
    async fn serve_tls(
        addr: SocketAddr,
        state: Arc<RpcState>,
        tls_config: RpcTlsConfig,
        mut shutdown: watch::Receiver<bool>,
    ) {
        use axum::extract::ConnectInfo;
        use hyper_util::rt::TokioIo;

        let app = Self::router(state);

        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(error = %e, "failed to bind RPC TLS server");
                return;
            }
        };

        info!(addr = %addr, "RPC server listening (HTTPS)");
        metrics::counter!("nusantara_rpc_server_started").increment(1);

        let acceptor = tls_config.acceptor;

        loop {
            tokio::select! {
                biased;
                _ = shutdown.wait_for(|v| *v) => {
                    info!("RPC TLS server shutting down");
                    break;
                }
                result = listener.accept() => {
                    let (tcp_stream, remote_addr) = match result {
                        Ok(conn) => conn,
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to accept TCP connection");
                            continue;
                        }
                    };

                    let acceptor = acceptor.clone();
                    let mut app = app.clone();

                    // Inject ConnectInfo so the rate limiter can access the client IP
                    // even though we are not using axum::serve (which normally does this).
                    app = app.layer(axum::Extension(ConnectInfo(remote_addr)));

                    tokio::spawn(async move {
                        let tls_stream = match acceptor.accept(tcp_stream).await {
                            Ok(s) => s,
                            Err(e) => {
                                tracing::debug!(
                                    remote = %remote_addr,
                                    error = %e,
                                    "TLS handshake failed"
                                );
                                return;
                            }
                        };

                        let io = TokioIo::new(tls_stream);
                        let service = hyper_util::service::TowerToHyperService::new(app);

                        if let Err(e) = hyper_util::server::conn::auto::Builder::new(
                            hyper_util::rt::TokioExecutor::new(),
                        )
                        .serve_connection(io, service)
                        .await
                        {
                            tracing::debug!(
                                remote = %remote_addr,
                                error = %e,
                                "connection error"
                            );
                        }
                    });
                }
            }
        }
    }
}
