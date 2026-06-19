use std::net::SocketAddr;
use std::sync::Arc;

use nusantara_core::native_token::const_parse_u64;
use nusantara_core::transaction::Transaction;
use nusantara_crypto::Hash;
use tokio::sync::{mpsc, watch};
use tracing::{error, info, instrument};

use crate::connection_cache::ConnectionCache;
use crate::error::TpuError;
use crate::forwarder::TransactionForwarder;
use crate::quic_client::TpuQuicClient;
use crate::quic_server::TpuQuicServer;
use crate::rate_limiter::RateLimiter;

/// Ingress channel capacity between QUIC server and forwarder.
const INGRESS_CHANNEL_CAPACITY: usize =
    const_parse_u64(env!("NUSA_TPU_INGRESS_CHANNEL_CAPACITY")) as usize;

/// Janitor period for purging expired rate-limiter entries and closed connections.
const JANITOR_INTERVAL_MS: u64 = const_parse_u64(env!("NUSA_TPU_JANITOR_INTERVAL_MS"));

pub struct TpuService;

impl TpuService {
    /// Create and run the TPU service.
    ///
    /// Spawns four tasks:
    /// 1. QUIC server (accept + validate + rate-limit incoming txs)
    /// 2. Transaction forwarder (batch + route to leader / local)
    /// 3. Rate-limiter janitor (purge expired IP windows)
    /// 4. Connection-cache janitor (prune closed QUIC connections)
    ///
    /// All tasks respect the shutdown signal.  A panic in any task is detected
    /// and propagated so the caller can tear down cleanly.
    #[instrument(skip_all, name = "tpu_service")]
    pub async fn run<F>(
        server_endpoint: quinn::Endpoint,
        client_endpoint: quinn::Endpoint,
        my_identity: Hash,
        local_tx_sender: mpsc::Sender<Transaction>,
        leader_lookup: F,
        shutdown: watch::Receiver<bool>,
    ) where
        F: Fn() -> Option<(Hash, SocketAddr)> + Send + Sync + 'static,
    {
        let rate_limiter = Arc::new(RateLimiter::new());
        let connection_cache = Arc::new(ConnectionCache::new());

        let (ingress_tx, ingress_rx) = mpsc::channel::<Transaction>(INGRESS_CHANNEL_CAPACITY);

        let server = TpuQuicServer::new(server_endpoint, Arc::clone(&rate_limiter));
        let client = Arc::new(TpuQuicClient::new(
            client_endpoint,
            Arc::clone(&connection_cache),
        ));
        let forwarder = TransactionForwarder::new(my_identity, client);

        let shutdown_server = shutdown.clone();
        let shutdown_forwarder = shutdown.clone();
        let shutdown_rl_janitor = shutdown.clone();
        let shutdown_cc_janitor = shutdown.clone();

        // Task 1 — QUIC server.
        let server_handle = tokio::spawn(async move {
            server.run(ingress_tx, shutdown_server).await;
        });

        // Task 2 — transaction forwarder.
        let forwarder_handle = tokio::spawn(async move {
            forwarder
                .run(
                    ingress_rx,
                    local_tx_sender,
                    leader_lookup,
                    shutdown_forwarder,
                )
                .await;
        });

        // Task 3 — rate-limiter janitor.
        let rl = Arc::clone(&rate_limiter);
        let rl_janitor_handle = tokio::spawn(async move {
            let period = tokio::time::Duration::from_millis(JANITOR_INTERVAL_MS);
            let mut ticker = tokio::time::interval(period);
            let mut sd = shutdown_rl_janitor;
            loop {
                tokio::select! {
                    biased;
                    _ = ticker.tick() => { rl.purge_expired(); }
                    _ = sd.changed() => break,
                }
            }
        });

        // Task 4 — connection-cache janitor.
        let cc = Arc::clone(&connection_cache);
        let cc_janitor_handle = tokio::spawn(async move {
            let period = tokio::time::Duration::from_millis(JANITOR_INTERVAL_MS);
            let mut ticker = tokio::time::interval(period);
            let mut sd = shutdown_cc_janitor;
            loop {
                tokio::select! {
                    biased;
                    _ = ticker.tick() => { cc.prune_closed(); }
                    _ = sd.changed() => break,
                }
            }
        });

        // Wait for all tasks; propagate panics so the service tears down cleanly.
        let result = tokio::try_join!(
            server_handle,
            forwarder_handle,
            rl_janitor_handle,
            cc_janitor_handle,
        );

        if let Err(e) = result {
            error!(error = %e, "TPU service task panicked");
        }

        info!("TPU service stopped");
    }

    /// Create a self-signed TLS configuration for QUIC.
    pub fn create_server_config() -> Result<quinn::ServerConfig, TpuError> {
        let cert = rcgen::generate_simple_self_signed(vec!["nusantara".to_string()])
            .map_err(|e| TpuError::Tls(e.to_string()))?;

        let key_der = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
        let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());

        let server_config = quinn::ServerConfig::with_single_cert(vec![cert_der], key_der.into())
            .map_err(|e| TpuError::Tls(e.to_string()))?;

        Ok(server_config)
    }

    /// Create a client TLS configuration that skips TLS cert verification.
    ///
    /// Dilithium3 identity verification is performed at the application layer
    /// (via signed shreds and gossip CRDS entries), not at the TLS layer.
    pub fn create_client_config() -> Result<quinn::ClientConfig, TpuError> {
        let crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
            .with_no_client_auth();

        let client_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
                .map_err(|e| TpuError::Tls(e.to_string()))?,
        ));

        Ok(client_config)
    }
}

/// Skip TLS certificate verification — identity is verified at the app layer.
#[derive(Debug)]
struct SkipServerVerification;

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}
