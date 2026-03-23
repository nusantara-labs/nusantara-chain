use std::net::SocketAddr;
use std::sync::Arc;

use nusantara_core::transaction::Transaction;
use nusantara_crypto::Hash;
use tokio::sync::{mpsc, watch};
use tracing::{info, instrument};

use crate::connection_cache::ConnectionCache;
use crate::error::TpuError;
use crate::forwarder::TransactionForwarder;
use crate::quic_client::TpuQuicClient;
use crate::quic_server::TpuQuicServer;
use crate::rate_limiter::RateLimiter;

pub struct TpuService;

impl TpuService {
    /// Create and run the TPU service.
    ///
    /// - `server_endpoint`: Quinn endpoint for incoming connections
    /// - `client_endpoint`: Quinn endpoint for outgoing connections
    /// - `my_identity`: This node's identity hash
    /// - `local_tx_sender`: Channel to send transactions to local BlockProducer
    /// - `leader_lookup`: Function returning current leader identity + TPU address
    /// - `shutdown`: Shutdown signal
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

        // Channel from QUIC server -> forwarder
        let (ingress_tx, ingress_rx) = mpsc::channel::<Transaction>(10_000);

        // QUIC server for incoming transactions
        let server = TpuQuicServer::new(server_endpoint, Arc::clone(&rate_limiter));

        // QUIC client for forwarding
        let client = Arc::new(TpuQuicClient::new(client_endpoint, connection_cache));

        // Forwarder
        let forwarder = TransactionForwarder::new(my_identity, client);

        let shutdown_server = shutdown.clone();
        let shutdown_forwarder = shutdown.clone();

        // Run server and forwarder concurrently
        let server_handle = tokio::spawn(async move {
            server.run(ingress_tx, shutdown_server).await;
        });

        let forwarder_handle = tokio::spawn(async move {
            forwarder
                .run(ingress_rx, local_tx_sender, leader_lookup, shutdown_forwarder)
                .await;
        });

        let _ = tokio::join!(server_handle, forwarder_handle);

        info!("TPU service stopped");
    }

    /// Create a self-signed TLS configuration for QUIC.
    pub fn create_server_config() -> Result<quinn::ServerConfig, TpuError> {
        let cert = rcgen::generate_simple_self_signed(vec!["nusantara".to_string()])
            .map_err(|e| TpuError::Tls(e.to_string()))?;

        let key_der = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
        let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());

        let server_config =
            quinn::ServerConfig::with_single_cert(vec![cert_der], key_der.into())
                .map_err(|e| TpuError::Tls(e.to_string()))?;

        Ok(server_config)
    }

    /// Create a client TLS configuration that accepts any certificate.
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

/// Skip TLS certificate verification (we do Dilithium3 identity verification at app layer).
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
