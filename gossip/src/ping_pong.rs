use std::time::Instant;

use dashmap::DashMap;
use nusantara_crypto::{Hash, Keypair, PublicKey, hash as crypto_hash, hashv};

use crate::protocol::{PingMessage, PongMessage};

pub struct PingCache {
    verified: DashMap<Hash, Instant>,
    /// Pending pong responses: peer_identity -> (ping_token, sent_at)
    pending: DashMap<Hash, (Hash, Instant)>,
    ttl: std::time::Duration,
}

impl PingCache {
    pub fn new(ttl_ms: u64) -> Self {
        Self {
            verified: DashMap::new(),
            pending: DashMap::new(),
            ttl: std::time::Duration::from_millis(ttl_ms),
        }
    }

    pub fn is_verified(&self, identity: &Hash) -> bool {
        if let Some(entry) = self.verified.get(identity) {
            entry.elapsed() < self.ttl
        } else {
            false
        }
    }

    pub fn mark_verified(&self, identity: Hash) {
        self.verified.insert(identity, Instant::now());
    }

    /// Create a ping message targeting a specific peer identity.
    /// Stores the token in `pending` so the response can be verified.
    pub fn create_ping(&self, keypair: &Keypair, target: Hash) -> PingMessage {
        let token = crypto_hash(&rand::random::<[u8; 32]>());
        self.pending.insert(target, (token, Instant::now()));
        let sig = keypair.sign(token.as_bytes());
        PingMessage {
            from: keypair.address(),
            token,
            signature: sig,
        }
    }

    pub fn create_pong(keypair: &Keypair, ping: &PingMessage) -> PongMessage {
        let token_hash = crypto_hash(ping.token.as_bytes());
        let sign_data = hashv(&[b"pong", token_hash.as_bytes()]);
        let sig = keypair.sign(sign_data.as_bytes());
        PongMessage {
            from: keypair.address(),
            token_hash,
            signature: sig,
        }
    }

    /// Verify a pong response:
    /// 1. Check that we have a pending ping for this peer
    /// 2. Verify the token hash matches
    /// 3. Verify the pong signature against the peer's public key
    pub fn verify_pong(&self, pong: &PongMessage, pubkey: &PublicKey) -> bool {
        let pending_token = match self.pending.remove(&pong.from) {
            Some((_, (token, sent_at))) => {
                if sent_at.elapsed() >= self.ttl {
                    metrics::counter!("nusantara_gossip_pong_expired_total").increment(1);
                    return false;
                }
                token
            }
            None => {
                metrics::counter!("nusantara_gossip_pong_unsolicited_total").increment(1);
                return false;
            }
        };

        let expected_hash = crypto_hash(pending_token.as_bytes());
        if pong.token_hash != expected_hash {
            metrics::counter!("nusantara_gossip_pong_verification_failed_total").increment(1);
            return false;
        }

        // Verify signature: pong signs hashv(&[b"pong", token_hash])
        let sign_data = hashv(&[b"pong", pong.token_hash.as_bytes()]);
        if pong.signature.verify(pubkey, sign_data.as_bytes()).is_err() {
            metrics::counter!("nusantara_gossip_pong_verification_failed_total").increment(1);
            return false;
        }

        true
    }

    /// Verify a ping message signature against the sender's public key.
    pub fn verify_ping(ping: &PingMessage, pubkey: &PublicKey) -> bool {
        if ping.signature.verify(pubkey, ping.token.as_bytes()).is_err() {
            metrics::counter!("nusantara_gossip_ping_invalid_signature_total").increment(1);
            return false;
        }
        true
    }

    pub fn purge_expired(&self) {
        self.verified.retain(|_, instant| instant.elapsed() < self.ttl);
        self.purge_expired_pending();
    }

    /// Clean up stale pending pong entries.
    pub fn purge_expired_pending(&self) {
        self.pending.retain(|_, (_, sent_at)| sent_at.elapsed() < self.ttl);
    }

    pub fn len(&self) -> usize {
        self.verified.len()
    }

    pub fn is_empty(&self) -> bool {
        self.verified.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_ping_and_pong() {
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();
        let cache = PingCache::new(60_000);

        let target = kp2.address();
        let ping = cache.create_ping(&kp1, target);
        assert_eq!(ping.from, kp1.address());

        let pong = PingCache::create_pong(&kp2, &ping);
        assert_eq!(pong.from, kp2.address());

        // Pong should verify with kp2's pubkey
        assert!(cache.verify_pong(&pong, kp2.public_key()));
    }

    #[test]
    fn wrong_pubkey_pong_rejected() {
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();
        let kp3 = Keypair::generate();
        let cache = PingCache::new(60_000);

        let ping = cache.create_ping(&kp1, kp2.address());
        let pong = PingCache::create_pong(&kp2, &ping);

        // Verifying with wrong pubkey should fail
        assert!(!cache.verify_pong(&pong, kp3.public_key()));
    }

    #[test]
    fn unsolicited_pong_rejected() {
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();
        let cache = PingCache::new(60_000);

        // Create a pong without a corresponding ping
        let fake_ping = PingMessage {
            from: kp1.address(),
            token: crypto_hash(b"fake_token"),
            signature: kp1.sign(crypto_hash(b"fake_token").as_bytes()),
        };
        let pong = PingCache::create_pong(&kp2, &fake_ping);

        // No pending entry, should be rejected
        assert!(!cache.verify_pong(&pong, kp2.public_key()));
    }

    #[test]
    fn expired_pending_rejected() {
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();
        let cache = PingCache::new(0); // 0ms TTL

        let ping = cache.create_ping(&kp1, kp2.address());
        let pong = PingCache::create_pong(&kp2, &ping);

        std::thread::sleep(std::time::Duration::from_millis(1));
        assert!(!cache.verify_pong(&pong, kp2.public_key()));
    }

    #[test]
    fn verify_ping_valid() {
        let kp = Keypair::generate();
        let cache = PingCache::new(60_000);
        let ping = cache.create_ping(&kp, Hash::zero());

        assert!(PingCache::verify_ping(&ping, kp.public_key()));
    }

    #[test]
    fn verify_ping_wrong_key() {
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();
        let cache = PingCache::new(60_000);
        let ping = cache.create_ping(&kp1, Hash::zero());

        assert!(!PingCache::verify_ping(&ping, kp2.public_key()));
    }

    #[test]
    fn verified_cache() {
        let cache = PingCache::new(60_000);
        let identity = crypto_hash(b"node");

        assert!(!cache.is_verified(&identity));
        cache.mark_verified(identity);
        assert!(cache.is_verified(&identity));
    }

    #[test]
    fn expired_entry_not_verified() {
        let cache = PingCache::new(0); // 0ms TTL
        let identity = crypto_hash(b"node");

        cache.mark_verified(identity);
        std::thread::sleep(std::time::Duration::from_millis(1));
        assert!(!cache.is_verified(&identity));
    }
}
