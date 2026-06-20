use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use nusantara_core::native_token::const_parse_u64;
use nusantara_crypto::{hash as crypto_hash, hashv};
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tracing::{debug, error, info, instrument};

use crate::cluster_info::ClusterInfo;
use crate::crds_value::CrdsValue;
use crate::protocol::{GossipMessage, PushMessage};

pub const PUSH_INTERVAL_MS: u64 = const_parse_u64(env!("NUSA_GOSSIP_PUSH_INTERVAL_MS"));
pub const PULL_INTERVAL_MS: u64 = const_parse_u64(env!("NUSA_GOSSIP_PULL_INTERVAL_MS"));
pub const PURGE_TIMEOUT_MS: u64 = const_parse_u64(env!("NUSA_GOSSIP_PURGE_TIMEOUT_MS"));
pub const RECV_RATE_LIMIT_PER_IP_PER_SEC: u64 =
    const_parse_u64(env!("NUSA_GOSSIP_RECV_RATE_LIMIT_PER_IP_PER_SEC"));

const MAX_UDP_PACKET: usize = 65507;

/// Maximum number of tracked IPs in the rate limiter.
const MAX_RATE_LIMITER_ENTRIES: usize = 100_000;

/// Parallelism bound for Dilithium3 signature verification tasks.
/// Prevents spawning unlimited blocking threads on a push flood (C4).
fn verify_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

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

    fn check(&self, ip: IpAddr) -> bool {
        if self.counts.len() >= MAX_RATE_LIMITER_ENTRIES && !self.counts.contains_key(&ip) {
            // Evict oldest entry before rejecting, giving new IPs a chance (M4).
            let oldest = self
                .counts
                .iter()
                .min_by_key(|e| e.value().0)
                .map(|e| *e.key());
            if let Some(k) = oldest {
                if self
                    .counts
                    .get(&k)
                    .map(|e| e.0.elapsed().as_secs() >= 2)
                    .unwrap_or(false)
                {
                    self.counts.remove(&k);
                } else {
                    metrics::counter!("nusantara_gossip_rate_limiter_capacity_exceeded")
                        .increment(1);
                    return false;
                }
            }
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

    fn purge_expired(&self) {
        self.counts
            .retain(|_, (instant, _)| instant.elapsed().as_secs() < 2);
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

    /// Run all gossip tasks until shutdown. Uses JoinSet for RAII task cleanup (L10).
    #[instrument(skip(self, shutdown), name = "gossip_service")]
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        let socket = self.socket;
        let cluster_info = self.cluster_info;

        let rate_limiter = Arc::new(GossipRateLimiter::new(
            RECV_RATE_LIMIT_PER_IP_PER_SEC as u32,
        ));
        let pong_limiter = Arc::new(GossipRateLimiter::new(10));

        let verify_sem = Arc::new(tokio::sync::Semaphore::new(verify_parallelism()));

        let mut tasks: JoinSet<&'static str> = JoinSet::new();

        // Recv task
        {
            let socket = Arc::clone(&socket);
            let ci = Arc::clone(&cluster_info);
            let rl = Arc::clone(&rate_limiter);
            let pl = Arc::clone(&pong_limiter);
            let sem = Arc::clone(&verify_sem);
            let mut sd = shutdown.clone();
            tasks.spawn(async move {
                recv_loop(socket, ci, rl, pl, sem, &mut sd).await;
                "recv"
            });
        }

        // Push task
        {
            let socket = Arc::clone(&socket);
            let ci = Arc::clone(&cluster_info);
            let mut sd = shutdown.clone();
            tasks.spawn(async move {
                push_loop(socket, ci, &mut sd).await;
                "push"
            });
        }

        // Pull task
        {
            let socket = Arc::clone(&socket);
            let ci = Arc::clone(&cluster_info);
            let mut sd = shutdown.clone();
            tasks.spawn(async move {
                pull_loop(socket, ci, &mut sd).await;
                "pull"
            });
        }

        // Purge task
        {
            let ci = Arc::clone(&cluster_info);
            let rl = Arc::clone(&rate_limiter);
            let pl = Arc::clone(&pong_limiter);
            let mut sd = shutdown.clone();
            tasks.spawn(async move {
                purge_loop(ci, rl, pl, &mut sd).await;
                "purge"
            });
        }

        // Self-refresh task
        {
            let ci = Arc::clone(&cluster_info);
            let mut sd = shutdown.clone();
            tasks.spawn(async move {
                refresh_loop(ci, &mut sd).await;
                "refresh"
            });
        }

        // Initial entrypoint pull — inside JoinSet so shutdown aborts it (L12).
        let entrypoints = cluster_info.entrypoints().to_vec();
        if !entrypoints.is_empty() {
            let ep_socket = Arc::clone(&socket);
            let ep_ci = Arc::clone(&cluster_info);
            tasks.spawn(async move {
                for ep in &entrypoints {
                    let ci = ep_ci.my_contact_info();
                    let req = ep_ci
                        .pull()
                        .build_pull_request(ep_ci.crds(), ep_ci.keypair(), &ci);
                    let msg = GossipMessage::PullRequest(req);
                    if let Ok(bytes) = msg.serialize_to_bytes() {
                        let _ = ep_socket.send_to(&bytes, ep).await;
                    }
                }
                info!(
                    count = entrypoints.len(),
                    "sent initial pull to entrypoints"
                );
                "entrypoint_pull"
            });
        }

        let _ = shutdown.changed().await;
        // Dropping JoinSet aborts all tasks (L10 RAII).
        drop(tasks);

        info!("gossip service stopped");
    }
}

async fn recv_loop(
    socket: Arc<UdpSocket>,
    cluster_info: Arc<ClusterInfo>,
    rate_limiter: Arc<GossipRateLimiter>,
    pong_limiter: Arc<GossipRateLimiter>,
    verify_sem: Arc<tokio::sync::Semaphore>,
    shutdown: &mut watch::Receiver<bool>,
) {
    let mut buf = vec![0u8; MAX_UDP_PACKET];
    loop {
        tokio::select! {
            biased;
            result = socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, src)) => {
                        if !rate_limiter.check(src.ip()) {
                            metrics::counter!("nusantara_gossip_rate_limited_total").increment(1);
                            continue;
                        }

                        let data = buf[..len].to_vec();
                        let socket = Arc::clone(&socket);
                        let ci = Arc::clone(&cluster_info);
                        let pl = Arc::clone(&pong_limiter);
                        let sem = Arc::clone(&verify_sem);

                        // Bound concurrent verify tasks (C4).
                        let permit = match sem.try_acquire_owned() {
                            Ok(p) => p,
                            Err(_) => {
                                metrics::counter!("nusantara_gossip_verify_backpressure_total").increment(1);
                                continue;
                            }
                        };

                        tokio::spawn(async move {
                            let _permit = permit;
                            if let Err(e) = handle_message(&socket, ci, &pl, &data, src).await {
                                debug!(%src, error = %e, "failed to handle gossip message");
                            }
                        });
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
    cluster_info: Arc<ClusterInfo>,
    pong_limiter: &GossipRateLimiter,
    data: &[u8],
    src: SocketAddr,
) -> Result<(), String> {
    let msg = GossipMessage::deserialize_from_bytes(data)?;
    metrics::counter!("nusantara_gossip_messages_received_total").increment(1);

    match msg {
        GossipMessage::PushMessage(push) => {
            // Require ping-verified sender before processing (C4, C3 analog for push).
            if !cluster_info.ping_cache().is_verified(&push.from) {
                metrics::counter!("nusantara_gossip_push_unverified_peer_dropped_total")
                    .increment(1);
                // Send a ping to trigger the handshake.
                let ping = cluster_info
                    .ping_cache()
                    .create_ping(cluster_info.keypair(), push.from);
                let msg = GossipMessage::Ping(ping);
                if let Ok(bytes) = msg.serialize_to_bytes() {
                    let _ = socket.send_to(&bytes, src).await;
                }
                return Ok(());
            }

            // Offload Dilithium3 verification to a blocking thread (C4, H6).
            // Arc::clone gives the blocking task its own reference count — no raw
            // pointer or lifetime extension needed.
            let values = push.values;
            let ci = Arc::clone(&cluster_info);
            let verified_values = tokio::task::spawn_blocking(move || {
                values
                    .into_iter()
                    .filter(|v| {
                        let pubkey = match &v.data {
                            crate::crds_value::CrdsData::ContactInfo(info) => {
                                Some(info.pubkey.clone())
                            }
                            _ => ci.get_pubkey(&v.origin()),
                        };
                        pubkey.map(|pk| v.verify(&pk)).unwrap_or(false)
                    })
                    .collect::<Vec<CrdsValue>>()
            })
            .await
            .map_err(|e| e.to_string())?;

            for value in verified_values {
                cluster_info.crds().insert(value).ok();
            }
            metrics::counter!("nusantara_gossip_push_received_total").increment(1);
        }
        GossipMessage::PullRequest(req) => {
            cluster_info.insert_verified_crds_value(req.self_value.clone());

            // Require ping-verified sender before responding to prevent UDP amplification (C3).
            let origin = req.self_value.origin();
            if !cluster_info.ping_cache().is_verified(&origin) {
                metrics::counter!("nusantara_gossip_pull_unverified_peer_dropped_total")
                    .increment(1);
                let ping = cluster_info
                    .ping_cache()
                    .create_ping(cluster_info.keypair(), origin);
                let msg = GossipMessage::Ping(ping);
                if let Ok(bytes) = msg.serialize_to_bytes() {
                    let _ = socket.send_to(&bytes, src).await;
                }
                return Ok(());
            }

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
            // Offload batch sig verification to blocking thread (H6).
            let crds = Arc::clone(cluster_info.crds());
            let values = resp.values;
            let verified_values = tokio::task::spawn_blocking(move || {
                values
                    .into_iter()
                    .filter(|v| {
                        let pubkey = match &v.data {
                            crate::crds_value::CrdsData::ContactInfo(info) => {
                                Some(info.pubkey.clone())
                            }
                            _ => crds
                                .get_contact_info(&v.origin())
                                .map(|ci| ci.pubkey.clone()),
                        };
                        pubkey.map(|pk| v.verify(&pk)).unwrap_or(false)
                    })
                    .collect::<Vec<CrdsValue>>()
            })
            .await
            .map_err(|e| e.to_string())?;

            for value in verified_values {
                cluster_info.crds().insert(value).ok();
            }
            metrics::counter!("nusantara_gossip_pull_responses_received_total").increment(1);
        }
        GossipMessage::PruneMessage(prune) => {
            if let Some(pubkey) = cluster_info.get_pubkey(&prune.from) {
                let sign_data = hashv(&[
                    b"prune",
                    &borsh::to_vec(&prune.prunes).expect("Vec<Hash> serialization cannot fail"),
                    prune.destination.as_bytes(),
                    &prune.wallclock.to_le_bytes(),
                ]);
                if prune
                    .signature
                    .verify(&pubkey, sign_data.as_bytes())
                    .is_err()
                {
                    metrics::counter!("nusantara_gossip_prune_invalid_signature_total")
                        .increment(1);
                    return Ok(());
                }
            } else {
                metrics::counter!("nusantara_gossip_prune_invalid_signature_total").increment(1);
                return Ok(());
            }
            cluster_info.push().process_prune(
                prune.from,
                &prune.prunes,
                Duration::from_millis(PURGE_TIMEOUT_MS),
            );
        }
        GossipMessage::Ping(ping) => {
            // Reject pings from unknown senders — no ContactInfo in CRDS means we
            // haven't done a pull handshake with them yet. Responding would let an
            // attacker burn our Dilithium3 sign budget (~5 ms/op) at a fraction of
            // the cost, even with the pong rate limiter in place.
            let Some(pubkey) = cluster_info.get_pubkey(&ping.from) else {
                metrics::counter!("nusantara_gossip_ping_unknown_sender_total").increment(1);
                return Ok(());
            };
            if !crate::ping_pong::PingCache::verify_ping(&ping, &pubkey) {
                metrics::counter!("nusantara_gossip_ping_invalid_signature_total").increment(1);
                return Ok(());
            }
            if !pong_limiter.check(src.ip()) {
                metrics::counter!("nusantara_gossip_pong_rate_limited_total").increment(1);
                return Ok(());
            }
            let pong = crate::ping_pong::PingCache::create_pong(cluster_info.keypair(), &ping);
            let msg = GossipMessage::Pong(pong);
            if let Ok(bytes) = msg.serialize_to_bytes() {
                let _ = socket.send_to(&bytes, src).await;
            }
        }
        GossipMessage::Pong(pong) => {
            if let Some(pubkey) = cluster_info.get_pubkey(&pong.from) {
                if cluster_info.ping_cache().verify_pong(&pong, &pubkey) {
                    cluster_info.ping_cache().mark_verified(pong.from);
                } else {
                    metrics::counter!("nusantara_gossip_pong_verification_failed_total")
                        .increment(1);
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
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

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
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            biased;
            _ = tick.tick() => {
                let peers = cluster_info.all_peers();
                let addr = if peers.is_empty() {
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

async fn refresh_loop(cluster_info: Arc<ClusterInfo>, shutdown: &mut watch::Receiver<bool>) {
    let interval = tokio::time::Duration::from_millis(PURGE_TIMEOUT_MS / 3);
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

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
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            biased;
            _ = tick.tick() => {
                let now_sys = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let min_wallclock = now_sys.saturating_sub(PURGE_TIMEOUT_MS);

                let purged = cluster_info.crds().purge_old(min_wallclock);
                if purged > 0 {
                    debug!(purged, "purged stale CRDS entries");
                }

                cluster_info.ping_cache().purge_expired();
                cluster_info.push().purge_expired_prunes(Instant::now());

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
        assert!(rl.check(ip2));
    }

    #[test]
    fn rate_limiter_resets_after_window() {
        let rl = GossipRateLimiter::new(1);
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(rl.check(ip));
        assert!(!rl.check(ip));
    }

    #[test]
    fn rate_limiter_purge_expired() {
        let rl = GossipRateLimiter::new(100);
        let ip1: IpAddr = "1.2.3.4".parse().unwrap();
        let ip2: IpAddr = "5.6.7.8".parse().unwrap();

        rl.check(ip1);
        rl.check(ip2);
        assert_eq!(rl.counts.len(), 2);

        rl.purge_expired();
        assert_eq!(rl.counts.len(), 2);

        // Manually backdate ip1's window to > 2s ago
        if let Some(mut e) = rl.counts.get_mut(&ip1) {
            e.0 = Instant::now() - Duration::from_secs(3);
        }
        rl.purge_expired();
        assert_eq!(rl.counts.len(), 1);
        assert!(rl.counts.contains_key(&ip2));
    }
}
