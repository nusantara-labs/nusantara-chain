use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

use axum::Router;
use dashmap::DashMap;
use lru::LruCache;
use nusantara_consensus::bank::ConsensusBank;
use nusantara_consensus::leader_schedule::{LeaderSchedule, LeaderScheduleGenerator};
use nusantara_core::Transaction;
use nusantara_core::epoch::EpochSchedule;
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

// ---------------------------------------------------------------------------
// Build-time constants (populated via build.rs + config.toml)
// ---------------------------------------------------------------------------

use nusantara_core::native_token::const_parse_u64;

/// Broadcast channel capacity for pubsub events.
pub const PUBSUB_CHANNEL_CAPACITY: usize =
    const_parse_u64(env!("NUSA_RPC_PUBSUB_CHANNEL_CAPACITY")) as usize;

/// Maximum concurrent WebSocket connections.
pub const MAX_WS_CONNECTIONS: usize =
    const_parse_u64(env!("NUSA_RPC_WS_MAX_CONNECTIONS")) as usize;

/// Maximum subscriptions per WebSocket connection.
pub const MAX_SUBSCRIPTIONS_PER_CONN: usize =
    const_parse_u64(env!("NUSA_RPC_WS_MAX_SUBSCRIPTIONS_PER_CONN")) as usize;

/// WebSocket send timeout in seconds.
pub const WS_SEND_TIMEOUT_SECS: u64 =
    const_parse_u64(env!("NUSA_RPC_WS_SEND_TIMEOUT_SECS"));

/// Faucet cooldown per recipient address (seconds).
pub const FAUCET_COOLDOWN_PER_ADDRESS_SECS: u64 =
    const_parse_u64(env!("NUSA_RPC_FAUCET_COOLDOWN_PER_ADDRESS_SECS"));

/// Faucet cooldown per source IP (seconds).
pub const FAUCET_COOLDOWN_PER_IP_SECS: u64 =
    const_parse_u64(env!("NUSA_RPC_FAUCET_COOLDOWN_PER_IP_SECS"));

/// Maximum lamports per airdrop.
pub const MAX_AIRDROP_LAMPORTS: u64 =
    const_parse_u64(env!("NUSA_RPC_FAUCET_MAX_AIRDROP_LAMPORTS"));

/// How often the faucet janitor runs (seconds).
pub const FAUCET_JANITOR_INTERVAL_SECS: u64 =
    const_parse_u64(env!("NUSA_RPC_FAUCET_JANITOR_INTERVAL_SECS"));

/// Maximum allowed timeout for confirm-style requests (ms).
pub const MAX_CONFIRM_TIMEOUT_MS: u64 =
    const_parse_u64(env!("NUSA_RPC_TRANSACTION_MAX_CONFIRM_TIMEOUT_MS"));

/// Slot lag threshold for health checks.
pub const HEALTH_BEHIND_SLOTS_THRESHOLD: u64 =
    const_parse_u64(env!("NUSA_RPC_HEALTH_BEHIND_SLOTS_THRESHOLD"));

/// Capacity of the LRU leader schedule cache (number of epochs).
pub const LEADER_CACHE_CAPACITY: usize =
    const_parse_u64(env!("NUSA_RPC_LEADER_CACHE_CAPACITY")) as usize;

/// Capacity of the snapshot file hash cache (number of entries).
pub const SNAPSHOT_CACHE_CAPACITY: usize =
    const_parse_u64(env!("NUSA_RPC_SNAPSHOT_CACHE_CAPACITY")) as usize;

/// Max JSON-RPC batch size.
pub const MAX_BATCH_SIZE: usize =
    const_parse_u64(env!("NUSA_RPC_JSONRPC_MAX_BATCH_SIZE")) as usize;

/// Rate limiter: max requests per second per IP.
pub const MAX_RPC_REQUESTS_PER_SECOND_PER_IP: u64 =
    const_parse_u64(env!("NUSA_RPC_RATE_LIMITER_MAX_REQUESTS_PER_SECOND_PER_IP"));

/// Rate limiter: max aggregate requests per second across all IPs.
pub const MAX_RPC_REQUESTS_PER_SECOND_GLOBAL: u64 =
    const_parse_u64(env!("NUSA_RPC_RATE_LIMITER_MAX_REQUESTS_PER_SECOND_GLOBAL"));

/// Rate limiter: how often the cleanup task runs (seconds).
pub const RATE_LIMITER_CLEANUP_INTERVAL_SECS: u64 =
    const_parse_u64(env!("NUSA_RPC_RATE_LIMITER_CLEANUP_INTERVAL_SECS"));

/// Rate limiter: entries older than this (seconds) are purged.
pub const RATE_LIMITER_STALE_ENTRY_TIMEOUT_SECS: u64 =
    const_parse_u64(env!("NUSA_RPC_RATE_LIMITER_STALE_ENTRY_TIMEOUT_SECS"));

// ---------------------------------------------------------------------------
// Leader cache type
// ---------------------------------------------------------------------------

/// LRU-bounded leader schedule cache keyed by epoch.
/// Capacity is fixed at `LEADER_CACHE_CAPACITY` epochs.
/// Wrapped in `parking_lot::Mutex` (never held across `.await`).
pub type LeaderLru = LruCache<u64, LeaderSchedule>;
pub type SharedLeaderCache = Arc<parking_lot::Mutex<LeaderLru>>;

/// Construct a new empty leader cache with the configured capacity.
pub fn new_leader_cache() -> SharedLeaderCache {
    Arc::new(parking_lot::Mutex::new(LruCache::new(
        std::num::NonZeroUsize::new(LEADER_CACHE_CAPACITY)
            .expect("LEADER_CACHE_CAPACITY must be > 0"),
    )))
}

// ---------------------------------------------------------------------------
// Pubsub events
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Faucet cooldown map types
// ---------------------------------------------------------------------------

pub type FaucetAddressCooldowns = Arc<DashMap<Hash, Instant>>;
pub type FaucetIpCooldowns = Arc<DashMap<IpAddr, Instant>>;

// ---------------------------------------------------------------------------
// Snapshot file cache entry
// ---------------------------------------------------------------------------

/// Cached result of hashing a snapshot file.
/// Keyed by (path, mtime, size) to skip rehashing unchanged files.
pub struct CachedSnapshotInfo {
    pub mtime: std::time::SystemTime,
    pub size: u64,
    pub hash: Hash,
}

// ---------------------------------------------------------------------------
// RpcState
// ---------------------------------------------------------------------------

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
    pub tx_forward_sender: Option<mpsc::Sender<Transaction>>,
    /// Broadcast sender for real-time pubsub events delivered over WebSocket.
    pub pubsub_tx: broadcast::Sender<PubsubEvent>,
    /// Directory where snapshot files are stored.
    pub snapshot_dir: PathBuf,
    /// Semaphore that bounds the number of concurrent WebSocket connections.
    pub ws_semaphore: Arc<Semaphore>,
    /// Per-address cooldown tracker for faucet requests.
    /// Key: recipient address Hash, Value: last airdrop timestamp.
    /// Using Hash (64 bytes) instead of String (~88 bytes) halves per-entry size.
    pub faucet_address_cooldowns: FaucetAddressCooldowns,
    /// Per-IP cooldown tracker for faucet requests.
    /// Key: source IP, Value: last airdrop timestamp.
    pub faucet_ip_cooldowns: FaucetIpCooldowns,
    /// LRU cache of recent snapshot file hashes to avoid rehashing on every request.
    /// Bounded to SNAPSHOT_CACHE_CAPACITY entries.
    pub snapshot_cache: Arc<parking_lot::Mutex<LruCache<PathBuf, CachedSnapshotInfo>>>,
}

impl RpcState {
    /// Create a new broadcast channel pair for pubsub events.
    pub fn new_pubsub_channel() -> broadcast::Sender<PubsubEvent> {
        let (tx, _rx) = broadcast::channel(PUBSUB_CHANNEL_CAPACITY);
        tx
    }

    /// Create a new WebSocket connection semaphore with the configured limit.
    pub fn new_ws_semaphore() -> Arc<Semaphore> {
        Arc::new(Semaphore::new(MAX_WS_CONNECTIONS))
    }

    /// Create a new pair of faucet cooldown maps (address + IP).
    ///
    /// Returns `(address_map, ip_map)` that can be stored in `RpcState`
    /// and also passed to `spawn_faucet_janitor`.
    pub fn new_faucet_cooldown_maps() -> (FaucetAddressCooldowns, FaucetIpCooldowns) {
        (Arc::new(DashMap::new()), Arc::new(DashMap::new()))
    }

    /// Create a new snapshot file hash cache.
    pub fn new_snapshot_cache() -> Arc<parking_lot::Mutex<LruCache<PathBuf, CachedSnapshotInfo>>> {
        Arc::new(parking_lot::Mutex::new(LruCache::new(
            std::num::NonZeroUsize::new(SNAPSHOT_CACHE_CAPACITY)
                .expect("SNAPSHOT_CACHE_CAPACITY must be > 0"),
        )))
    }

    /// Publish an event to all WebSocket subscribers.
    ///
    /// Ignores `SendError` (no subscribers) — callers must not rely on
    /// delivery confirmation. Increments the pubsub published metric.
    pub fn publish(&self, ev: PubsubEvent) {
        let _ = self.pubsub_tx.send(ev);
        metrics::counter!("nusantara_rpc_pubsub_published").increment(1);
    }

    /// Atomically claim the faucet cooldown for a recipient address.
    ///
    /// Inserts a fresh timestamp only when no unexpired entry exists.
    /// Returns `Ok(())` if the claim succeeded, `Err(RpcError::RateLimited)`
    /// if the address is still within cooldown.
    pub fn claim_faucet_address(&self, address_hash: &Hash) -> Result<(), crate::RpcError> {
        let now = Instant::now();
        let mut occupied = false;
        self.faucet_address_cooldowns
            .entry(*address_hash)
            .and_modify(|last| {
                if last.elapsed().as_secs() < FAUCET_COOLDOWN_PER_ADDRESS_SECS {
                    occupied = true;
                } else {
                    // Expired entry — overwrite with fresh timestamp to claim.
                    *last = now;
                }
            })
            .or_insert(now);

        if occupied {
            // The existing entry is still fresh; calculate remaining cooldown.
            let remaining = self
                .faucet_address_cooldowns
                .get(address_hash)
                .map(|e| {
                    FAUCET_COOLDOWN_PER_ADDRESS_SECS
                        .saturating_sub(e.elapsed().as_secs())
                })
                .unwrap_or(FAUCET_COOLDOWN_PER_ADDRESS_SECS);
            return Err(crate::RpcError::RateLimited(format!(
                "address cooldown: retry in {remaining}s"
            )));
        }
        Ok(())
    }

    /// Release an address cooldown claim previously acquired via
    /// `claim_faucet_address`. Called on transaction failure so the user
    /// can retry without waiting the full cooldown.
    pub fn release_faucet_address(&self, address_hash: &Hash) {
        self.faucet_address_cooldowns.remove(address_hash);
    }

    /// Atomically claim the faucet cooldown for a source IP.
    ///
    /// Localhost / Docker bridge IPs are exempt and always return `Ok(())`.
    /// Returns `Err(RpcError::RateLimited)` if the IP is still within cooldown.
    pub fn claim_faucet_ip(&self, ip: IpAddr) -> Result<(), crate::RpcError> {
        if crate::rate_limiter::is_local_or_docker(ip) {
            return Ok(());
        }
        let now = Instant::now();
        let mut occupied = false;
        self.faucet_ip_cooldowns
            .entry(ip)
            .and_modify(|last| {
                if last.elapsed().as_secs() < FAUCET_COOLDOWN_PER_IP_SECS {
                    occupied = true;
                } else {
                    *last = now;
                }
            })
            .or_insert(now);

        if occupied {
            let remaining = self
                .faucet_ip_cooldowns
                .get(&ip)
                .map(|e| {
                    FAUCET_COOLDOWN_PER_IP_SECS.saturating_sub(e.elapsed().as_secs())
                })
                .unwrap_or(FAUCET_COOLDOWN_PER_IP_SECS);
            return Err(crate::RpcError::RateLimited(format!(
                "IP cooldown: retry in {remaining}s"
            )));
        }
        Ok(())
    }

    /// Release an IP cooldown claim previously acquired via `claim_faucet_ip`.
    pub fn release_faucet_ip(&self, ip: IpAddr) {
        self.faucet_ip_cooldowns.remove(&ip);
    }

    /// Spawn the faucet janitor task.
    ///
    /// Purges expired faucet cooldown entries every `FAUCET_JANITOR_INTERVAL_SECS`
    /// seconds.  Entries are considered expired when their timestamp is older than
    /// 2× the applicable cooldown.  The task holds only a `Weak` reference to the
    /// `DashMap`s so it terminates automatically when `RpcState` is dropped.
    ///
    /// An explicit shutdown signal (`watch::Receiver<bool>`) can also terminate
    /// the task early when the validator shuts down cleanly.
    #[tracing::instrument(skip_all, name = "faucet_janitor")]
    pub fn spawn_faucet_janitor(
        address_cooldowns: FaucetAddressCooldowns,
        ip_cooldowns: FaucetIpCooldowns,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let addr_weak = Arc::downgrade(&address_cooldowns);
        let ip_weak = Arc::downgrade(&ip_cooldowns);

        tokio::spawn(async move {
            let interval_dur =
                std::time::Duration::from_secs(FAUCET_JANITOR_INTERVAL_SECS);
            let mut interval = tokio::time::interval(interval_dur);
            // Avoid a burst of back-to-back purges after a runtime pause.
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // The first tick fires immediately; skip it so we don't purge before
            // any entries are inserted.
            interval.tick().await;

            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.wait_for(|v| *v) => {
                        tracing::debug!("faucet janitor shutdown signal received");
                        break;
                    }
                    _ = interval.tick() => {}
                }

                // Upgrade weak references; exit if RpcState has been dropped.
                let Some(addr_map) = addr_weak.upgrade() else { break };
                let Some(ip_map) = ip_weak.upgrade() else { break };

                let addr_cutoff = 2 * FAUCET_COOLDOWN_PER_ADDRESS_SECS;
                let ip_cutoff = 2 * FAUCET_COOLDOWN_PER_IP_SECS;

                addr_map.retain(|_, last| last.elapsed().as_secs() < addr_cutoff);
                ip_map.retain(|_, last| last.elapsed().as_secs() < ip_cutoff);

                metrics::gauge!("nusantara_rpc_faucet_cooldown_entries", "kind" => "address")
                    .set(addr_map.len() as f64);
                metrics::gauge!("nusantara_rpc_faucet_cooldown_entries", "kind" => "ip")
                    .set(ip_map.len() as f64);

                tracing::debug!(
                    addr_entries = addr_map.len(),
                    ip_entries = ip_map.len(),
                    "faucet janitor purge complete"
                );
            }
        });
    }
}

// ---------------------------------------------------------------------------
// OpenAPI spec
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// TLS config
// ---------------------------------------------------------------------------

/// TLS configuration for HTTPS RPC.
pub struct RpcTlsConfig {
    acceptor: tokio_rustls::TlsAcceptor,
}

impl RpcTlsConfig {
    /// Build a TLS configuration from PEM-encoded certificate and key files.
    ///
    /// Uses `tokio::fs::read` (async) to avoid blocking the runtime.
    pub async fn from_pem_files(
        cert_path: &Path,
        key_path: &Path,
    ) -> Result<Self, crate::RpcError> {
        use rustls::pki_types::PrivateKeyDer;

        let cert_bytes = tokio::fs::read(cert_path).await.map_err(|e| {
            crate::RpcError::Internal(format!(
                "failed to read TLS cert {}: {e}",
                cert_path.display()
            ))
        })?;
        let key_bytes = tokio::fs::read(key_path).await.map_err(|e| {
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

        let key: PrivateKeyDer<'static> =
            rustls_pemfile::private_key(&mut key_bytes.as_slice())
                .map_err(|e| crate::RpcError::Internal(format!("invalid TLS key PEM: {e}")))?
                .ok_or_else(|| {
                    crate::RpcError::Internal(
                        "TLS key file contains no private key".to_string(),
                    )
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

// ---------------------------------------------------------------------------
// Router builder (F24 — free functions replacing empty struct)
// ---------------------------------------------------------------------------

/// Build the Axum `Router` for the RPC server.
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

/// Dispatch to TLS or plain HTTP depending on `tls`.
pub async fn serve(
    addr: SocketAddr,
    state: Arc<RpcState>,
    tls: Option<RpcTlsConfig>,
    shutdown: watch::Receiver<bool>,
) {
    if let Some(tls_config) = tls {
        serve_tls(addr, state, tls_config, shutdown).await;
    } else {
        serve_plain(addr, state, shutdown).await;
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
    let app = router(state);

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
/// Accepts TLS connections using `tokio_rustls::TlsAcceptor`, then hands the
/// decrypted stream to axum via `hyper`. A `tokio::task::JoinSet` tracks all
/// in-flight connection tasks so they drain gracefully on shutdown.
/// Each spawned connection task holds a clone of the shutdown receiver so it
/// can exit its read loop early.
///
/// `ConnectInfo` is injected manually from the `remote_addr` captured at
/// accept time so the rate limiter still has the client IP.
async fn serve_tls(
    addr: SocketAddr,
    state: Arc<RpcState>,
    tls_config: RpcTlsConfig,
    mut shutdown: watch::Receiver<bool>,
) {
    use axum::extract::ConnectInfo;
    use hyper_util::rt::TokioIo;
    use tokio::task::JoinSet;

    let app = router(state);

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
    let mut tasks: JoinSet<()> = JoinSet::new();
    // Clone shutdown once before the loop so we can both select! on it (mutable)
    // and clone it for individual connection tasks (immutable) without borrow
    // conflicts inside the select! macro.
    let conn_shutdown_template = shutdown.clone();

    loop {
        tokio::select! {
            biased;
            _ = shutdown.wait_for(|v| *v) => {
                info!("RPC TLS server shutting down, draining connections");
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
                let conn_shutdown = conn_shutdown_template.clone();

                // Inject ConnectInfo so the rate limiter can access the client IP.
                app = app.layer(axum::Extension(ConnectInfo(remote_addr)));

                tasks.spawn(async move {
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

                    // Drive the connection, respecting the shutdown signal.
                    // The builder must outlive the connection future, so bind
                    // it to a local variable before calling serve_connection.
                    let builder = hyper_util::server::conn::auto::Builder::new(
                        hyper_util::rt::TokioExecutor::new(),
                    );
                    let conn = builder.serve_connection(io, service);

                    // Pin the future so we can select! on it.
                    tokio::pin!(conn);
                    let mut shutdown = conn_shutdown;
                    loop {
                        tokio::select! {
                            biased;
                            _ = shutdown.wait_for(|v| *v) => {
                                // Initiate graceful HTTP connection close.
                                conn.as_mut().graceful_shutdown();
                            }
                            result = &mut conn => {
                                if let Err(e) = result {
                                    tracing::debug!(
                                        remote = %remote_addr,
                                        error = %e,
                                        "TLS connection error"
                                    );
                                }
                                break;
                            }
                        }
                    }
                });
            }
        }
    }

    // Drain all in-flight connection tasks.
    while let Some(result) = tasks.join_next().await {
        if let Err(e) = result {
            tracing::warn!(error = %e, "TLS connection task panicked");
        }
    }
    info!("RPC TLS server drained all connections");
}
