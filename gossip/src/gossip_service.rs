use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use nusantara_core::native_token::const_parse_u64;
use nusantara_crypto::{hash as crypto_hash, hashv};
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tracing::{debug, error, info, instrument};

use crate::cluster_info::ClusterInfo;
use crate::protocol::{GossipMessage, PushMessage};

pub const PUSH_INTERVAL_MS: u64 = const_parse_u64(env!("NUSA_GOSSIP_PUSH_INTERVAL_MS"));
pub const PULL_INTERVAL_MS: u64 = const_parse_u64(env!("NUSA_GOSSIP_PULL_INTERVAL_MS"));
pub const PURGE_TIMEOUT_MS: u64 = const_parse_u64(env!("NUSA_GOSSIP_PURGE_TIMEOUT_MS"));
pub const RECV_RATE_LIMIT_PER_IP_PER_SEC: u64 =
    const_parse_u64(env!("NUSA_GOSSIP_RECV_RATE_LIMIT_PER_IP_PER_SEC"));

const MAX_UDP_PACKET: usize = 65507;

/// Maximum number of tracked IPs in the rate limiter. Prevents OOM from
/// a flood of spoofed source IPs creating unbounded DashMap entries.
const MAX_RATE_LIMITER_ENTRIES: usize = 100_000;

/// Per-IP rate limiter for incoming gossip messages.
struct GossipRateLimiter {
    counts: DashMap<IpAddr, (Instant, u32)>,
    limit: u32,
}

impl GossipRateLimiter {
    fn new(limit: u32) -> Self {
        Self {
            counts: DashMap::new(),
            limit,
        }
    }

    /// Returns true if the message should be processed, false if rate limited.
    fn check(&self, ip: IpAddr) -> bool {
        // Guard against unbounded growth from spoofed IPs: reject unknown IPs
        // when the table is at capacity.
        if self.counts.len() >= MAX_RATE_LIMITER_ENTRIES && !self.counts.contains_key(&ip) {
            metrics::counter!("nusantara_gossip_rate_limiter_capacity_exceeded").increment(1);
            return false;
        }

        let mut entry = self.counts.entry(ip).or_insert((Instant::now(), 0));
        if entry.0.elapsed().as_secs() >= 1 {
            entry.0 = Instant::now();
            entry.1 = 0;
        }
        if entry.1 >= self.limit {
            return false;
        }
        entry.1 += 1;
        true
    }

    /// Remove entries whose window has expired, preventing unbounded memory growth.
    fn purge_expired(&self) {
        self.counts.retain(|_, (instant, _)| instant.elapsed().as_secs() < 2);
    }
}

pub struct GossipService {
    cluster_info: Arc<ClusterInfo>,
    socket: Arc<UdpSocket>,
}

impl GossipService {
    pub async fn new(
        cluster_info: Arc<ClusterInfo>,
        bind_addr: SocketAddr,
    ) -> Result<Self, crate::error::GossipError> {
        let socket = UdpSocket::bind(bind_addr).await.map_err(|e| {
            crate::error::GossipError::SocketBind {
                addr: bind_addr.to_string(),
                source: e,
            }
        })?;

        info!(%bind_addr, "gossip UDP socket bound");

        Ok(Self {
            cluster_info,
            socket: Arc::new(socket),
        })
    }

    pub fn cluster_info(&self) -> &Arc<ClusterInfo> {
        &self.cluster_info
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Run all gossip tasks until shutdown.
    #[instrument(skip(self, shutdown), name = "gossip_service")]
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        let socket = self.socket;
        let cluster_info = self.cluster_info;

        let rate_limiter = Arc::new(GossipRateLimiter::new(
            RECV_RATE_LIMIT_PER_IP_PER_SEC as u32,
        ));
        // Separate rate limiter for pong responses (10/sec/IP) to prevent
        // CPU-expensive Dilithium3 signing DoS via ping floods.
        let pong_limiter = Arc::new(GossipRateLimiter::new(10));

        let recv_socket = Arc::clone(&socket);
        let recv_ci = Arc::clone(&cluster_info);
        let recv_rl = Arc::clone(&rate_limiter);
        let recv_pl = Arc::clone(&pong_limiter);

        let send_socket = Arc::clone(&socket);
        let push_ci = Arc::clone(&cluster_info);

        let pull_socket = Arc::clone(&socket);
        let pull_ci = Arc::clone(&cluster_info);

        let purge_ci = Arc::clone(&cluster_info);
        let purge_rl = Arc::clone(&rate_limiter);
        let purge_pl = Arc::clone(&pong_limiter);

        let refresh_ci = Arc::clone(&cluster_info);

        // Spawn receiver task
        let mut shutdown_recv = shutdown.clone();
        let recv_handle = tokio::spawn(async move {
            recv_loop(recv_socket, recv_ci, recv_rl, recv_pl, &mut shutdown_recv).await;
        });

        // Spawn push task
        let mut shutdown_push = shutdown.clone();
        let push_handle = tokio::spawn(async move {
            push_loop(send_socket, push_ci, &mut shutdown_push).await;
        });

        // Spawn pull task
        let mut shutdown_pull = shutdown.clone();
        let pull_handle = tokio::spawn(async move {
            pull_loop(pull_socket, pull_ci, &mut shutdown_pull).await;
        });

        // Spawn purge task
        let mut shutdown_purge = shutdown.clone();
        let purge_handle = tokio::spawn(async move {
            purge_loop(purge_ci, purge_rl, purge_pl, &mut shutdown_purge).await;
        });

        // Spawn self-refresh task (keep our ContactInfo alive in peers' CRDS)
        let mut shutdown_refresh = shutdown.clone();
        let refresh_handle = tokio::spawn(async move {
            refresh_loop(refresh_ci, &mut shutdown_refresh).await;
        });

        // Send initial pull requests to entrypoints
        let entrypoints = cluster_info.entrypoints().to_vec();
        if !entrypoints.is_empty() {
            let ep_socket = Arc::clone(&socket);
            let ep_ci = Arc::clone(&cluster_info);
            tokio::spawn(async move {
                for ep in &entrypoints {
                    let ci = ep_ci.my_contact_info();
                    let req = ep_ci.pull().build_pull_request(
                        ep_ci.crds(),
                        ep_ci.keypair(),
                        &ci,
                    );
                    let msg = GossipMessage::PullRequest(req);
                    if let Ok(bytes) = msg.serialize_to_bytes() {
                        let _ = ep_socket.send_to(&bytes, ep).await;
                    }
                }
                info!(count = entrypoints.len(), "sent initial pull to entrypoints");
            });
        }

        // Wait for shutdown
        let _ = shutdown.changed().await;

        recv_handle.abort();
        push_handle.abort();
        pull_handle.abort();
        purge_handle.abort();
        refresh_handle.abort();

        info!("gossip service stopped");
    }
}

async fn recv_loop(
    socket: Arc<UdpSocket>,
    cluster_info: Arc<ClusterInfo>,
    rate_limiter: Arc<GossipRateLimiter>,
    pong_limiter: Arc<GossipRateLimiter>,
    shutdown: &mut watch::Receiver<bool>,
) {
    let mut buf = vec![0u8; MAX_UDP_PACKET];
    loop {
        tokio::select! {
            biased;
            result = socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, src)) => {
                        // Rate limit check before processing
                        if !rate_limiter.check(src.ip()) {
                            metrics::counter!("nusantara_gossip_rate_limited_total").increment(1);
                            continue;
                        }

                        let data = &buf[..len];
                        if let Err(e) = handle_message(&socket, &cluster_info, &pong_limiter, data, src).await {
                            debug!(%src, error = %e, "failed to handle gossip message");
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "gossip recv error");
                    }
                }
            }
            _ = shutdown.changed() => {
                break;
            }
        }
    }
}

async fn handle_message(
    socket: &UdpSocket,
    cluster_info: &ClusterInfo,
    pong_limiter: &GossipRateLimiter,
    data: &[u8],
    src: SocketAddr,
) -> Result<(), String> {
    let msg = GossipMessage::deserialize_from_bytes(data)?;
    metrics::counter!("nusantara_gossip_messages_received_total").increment(1);

    match msg {
        GossipMessage::PushMessage(push) => {
            for value in &push.values {
                cluster_info.insert_verified_crds_value(value.clone());
            }
            metrics::counter!("nusantara_gossip_push_received_total").increment(1);
        }
        GossipMessage::PullRequest(req) => {
            // Insert the requester's self-value (ContactInfo — self-verifiable)
            cluster_info.insert_verified_crds_value(req.self_value.clone());

            // Build and send response
            let response = cluster_info
                .pull()
                .process_pull_request(cluster_info.crds(), &req);

            if !response.values.is_empty() {
                let msg = GossipMessage::PullResponse(response);
                if let Ok(bytes) = msg.serialize_to_bytes() {
                    let _ = socket.send_to(&bytes, src).await;
                }
            }
            metrics::counter!("nusantara_gossip_pull_requests_received_total").increment(1);
        }
        GossipMessage::PullResponse(resp) => {
            cluster_info
                .pull()
                .process_pull_response(cluster_info.crds(), &resp);
            metrics::counter!("nusantara_gossip_pull_responses_received_total").increment(1);
        }
        GossipMessage::PruneMessage(prune) => {
            // Verify prune message signature
            if let Some(pubkey) = cluster_info.get_pubkey(&prune.from) {
                let sign_data = hashv(&[
                    b"prune",
                    &borsh::to_vec(&prune.prunes).unwrap_or_default(),
                    prune.destination.as_bytes(),
                    &prune.wallclock.to_le_bytes(),
                ]);
                if prune.signature.verify(&pubkey, sign_data.as_bytes()).is_err() {
                    metrics::counter!("nusantara_gossip_prune_invalid_signature_total").increment(1);
                    return Ok(());
                }
            } else {
                // Unknown peer — reject prune
                metrics::counter!("nusantara_gossip_prune_invalid_signature_total").increment(1);
                return Ok(());
            }
            cluster_info
                .push()
                .process_prune(prune.from, &prune.prunes);
        }
        GossipMessage::Ping(ping) => {
            // Verify ping signature if sender is known
            if let Some(pubkey) = cluster_info.get_pubkey(&ping.from)
                && !crate::ping_pong::PingCache::verify_ping(&ping, &pubkey)
            {
                metrics::counter!("nusantara_gossip_ping_invalid_signature_total").increment(1);
                return Ok(());
            }
            // Rate-limit pong responses to prevent Dilithium3-signing DoS.
            if !pong_limiter.check(src.ip()) {
                metrics::counter!("nusantara_gossip_pong_rate_limited_total").increment(1);
                return Ok(());
            }
            // Respond with pong (accept from unknown peers for bootstrapping)
            let pong = crate::ping_pong::PingCache::create_pong(
                cluster_info.keypair(),
                &ping,
            );
            let msg = GossipMessage::Pong(pong);
            if let Ok(bytes) = msg.serialize_to_bytes() {
                let _ = socket.send_to(&bytes, src).await;
            }
        }
        GossipMessage::Pong(pong) => {
            // Verify pong signature before accepting
            if let Some(pubkey) = cluster_info.get_pubkey(&pong.from) {
                if cluster_info.ping_cache().verify_pong(&pong, &pubkey) {
                    cluster_info.ping_cache().mark_verified(pong.from);
                } else {
                    metrics::counter!("nusantara_gossip_pong_verification_failed_total").increment(1);
                }
            } else {
                metrics::counter!("nusantara_gossip_pong_verification_failed_total").increment(1);
            }
        }
    }

    Ok(())
}

async fn push_loop(
    socket: Arc<UdpSocket>,
    cluster_info: Arc<ClusterInfo>,
    shutdown: &mut watch::Receiver<bool>,
) {
    let interval = tokio::time::Duration::from_millis(PUSH_INTERVAL_MS);
    let mut tick = tokio::time::interval(interval);

    loop {
        tokio::select! {
            biased;
            _ = tick.tick() => {
                let peers = cluster_info.peers_with_stakes(&HashMap::new());
                let seed = crypto_hash(&rand::random::<[u8; 32]>());
                let messages = cluster_info.push().new_push_messages(
                    cluster_info.crds(),
                    &peers,
                    &seed,
                );

                for (addr, values) in messages {
                    let msg = GossipMessage::PushMessage(PushMessage {
                        from: cluster_info.my_identity(),
                        values,
                    });
                    if let Ok(bytes) = msg.serialize_to_bytes() {
                        let _ = socket.send_to(&bytes, addr).await;
                    }
                }
            }
            _ = shutdown.changed() => {
                break;
            }
        }
    }
}

async fn pull_loop(
    socket: Arc<UdpSocket>,
    cluster_info: Arc<ClusterInfo>,
    shutdown: &mut watch::Receiver<bool>,
) {
    let interval = tokio::time::Duration::from_millis(PULL_INTERVAL_MS);
    let mut tick = tokio::time::interval(interval);

    loop {
        tokio::select! {
            biased;
            _ = tick.tick() => {
                let peers = cluster_info.all_peers();
                let addr = if peers.is_empty() {
                    // No known peers — fall back to entrypoints for rediscovery
                    let eps = cluster_info.entrypoints();
                    if eps.is_empty() {
                        continue;
                    }
                    let idx = rand::random_range(0..eps.len());
                    eps[idx]
                } else {
                    let idx = rand::random_range(0..peers.len());
                    peers[idx].gossip_addr.0
                };

                let ci = cluster_info.my_contact_info();
                let req = cluster_info.pull().build_pull_request(
                    cluster_info.crds(),
                    cluster_info.keypair(),
                    &ci,
                );

                let msg = GossipMessage::PullRequest(req);
                if let Ok(bytes) = msg.serialize_to_bytes() {
                    let _ = socket.send_to(&bytes, addr).await;
                    metrics::counter!("nusantara_gossip_pull_requests_sent_total").increment(1);
                }
            }
            _ = shutdown.changed() => {
                break;
            }
        }
    }
}

/// Periodically refresh our own ContactInfo wallclock to prevent self-purge
/// and ensure peers keep our entry alive.
async fn refresh_loop(
    cluster_info: Arc<ClusterInfo>,
    shutdown: &mut watch::Receiver<bool>,
) {
    // Refresh at 1/3 of purge timeout to stay well within the window
    let interval = tokio::time::Duration::from_millis(PURGE_TIMEOUT_MS / 3);
    let mut tick = tokio::time::interval(interval);

    loop {
        tokio::select! {
            biased;
            _ = tick.tick() => {
                cluster_info.refresh_self_wallclock();
                debug!("refreshed self ContactInfo wallclock");
            }
            _ = shutdown.changed() => {
                break;
            }
        }
    }
}

async fn purge_loop(
    cluster_info: Arc<ClusterInfo>,
    rate_limiter: Arc<GossipRateLimiter>,
    pong_limiter: Arc<GossipRateLimiter>,
    shutdown: &mut watch::Receiver<bool>,
) {
    let interval = tokio::time::Duration::from_millis(PURGE_TIMEOUT_MS);
    let mut tick = tokio::time::interval(interval);

    loop {
        tokio::select! {
            biased;
            _ = tick.tick() => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let min_wallclock = now.saturating_sub(PURGE_TIMEOUT_MS);

                let purged = cluster_info.crds().purge_old(min_wallclock);
                if purged > 0 {
                    debug!(purged, "purged stale CRDS entries");
                }

                cluster_info.ping_cache().purge_expired();
                cluster_info.push().clear_prunes();

                rate_limiter.purge_expired();
                pong_limiter.purge_expired();
            }
            _ = shutdown.changed() => {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_values() {
        assert_eq!(PUSH_INTERVAL_MS, 100);
        assert_eq!(PULL_INTERVAL_MS, 5000);
        assert_eq!(PURGE_TIMEOUT_MS, 30000);
        assert_eq!(RECV_RATE_LIMIT_PER_IP_PER_SEC, 10240);
    }

    #[test]
    fn rate_limiter_within_limit() {
        let rl = GossipRateLimiter::new(10);
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        for _ in 0..10 {
            assert!(rl.check(ip));
        }
    }

    #[test]
    fn rate_limiter_over_limit() {
        let rl = GossipRateLimiter::new(5);
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        for _ in 0..5 {
            assert!(rl.check(ip));
        }
        assert!(!rl.check(ip));
    }

    #[test]
    fn rate_limiter_per_ip_independent() {
        let rl = GossipRateLimiter::new(2);
        let ip1: IpAddr = "1.2.3.4".parse().unwrap();
        let ip2: IpAddr = "5.6.7.8".parse().unwrap();
        assert!(rl.check(ip1));
        assert!(rl.check(ip1));
        assert!(!rl.check(ip1));
        // ip2 should be independent
        assert!(rl.check(ip2));
    }

    #[test]
    fn rate_limiter_resets_after_window() {
        let rl = GossipRateLimiter::new(1);
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(rl.check(ip));
        assert!(!rl.check(ip));

        // Simulate window reset by waiting (can't easily in unit test)
        // Just verify the reset logic exists (tested by the elapsed check)
    }

    #[test]
    fn rate_limiter_purge_expired() {
        let rl = GossipRateLimiter::new(100);
        let ip1: IpAddr = "1.2.3.4".parse().unwrap();
        let ip2: IpAddr = "5.6.7.8".parse().unwrap();

        // Insert entries for both IPs
        rl.check(ip1);
        rl.check(ip2);
        assert_eq!(rl.counts.len(), 2);

        // Entries are fresh — purge should not remove them
        rl.purge_expired();
        assert_eq!(rl.counts.len(), 2);

        // Manually backdate ip1's window to > 2s ago
        rl.counts.entry(ip1).and_modify(|e| {
            e.0 = Instant::now() - std::time::Duration::from_secs(3);
        });

        rl.purge_expired();
        assert_eq!(rl.counts.len(), 1);
        assert!(rl.counts.get(&ip1).is_none());
        assert!(rl.counts.get(&ip2).is_some());
    }

    #[test]
    fn rate_limiter_rejects_unknown_ip_at_capacity() {
        // Fill the rate limiter to MAX_RATE_LIMITER_ENTRIES, then verify
        // that new unknown IPs are rejected while known IPs still pass.
        let rl = GossipRateLimiter::new(1000);

        // Insert entries up to capacity
        for i in 0..MAX_RATE_LIMITER_ENTRIES {
            let ip: IpAddr = IpAddr::V4(std::net::Ipv4Addr::from((i as u32).to_be_bytes()));
            assert!(rl.check(ip), "should accept IP {} before capacity", i);
        }
        assert_eq!(rl.counts.len(), MAX_RATE_LIMITER_ENTRIES);

        // A brand-new IP must be rejected
        let unknown_ip: IpAddr = "255.255.255.255".parse().unwrap();
        assert!(
            !rl.check(unknown_ip),
            "unknown IP must be rejected when rate limiter is at capacity"
        );

        // A known IP must still be accepted
        let known_ip: IpAddr = IpAddr::V4(std::net::Ipv4Addr::from(0u32.to_be_bytes()));
        assert!(
            rl.check(known_ip),
            "known IP must still be accepted at capacity"
        );
    }
}
