use std::time::Instant;

use dashmap::DashMap;
use nusantara_crypto::{Hash, Keypair, PublicKey, hash as crypto_hash, hashv};

use crate::protocol::{PingMessage, PongMessage};

pub struct PingCache {
    verified: DashMap<Hash, Instant>,
    /// Pending pong responses keyed by (peer_identity, token_hash) so a forged
    /// `from` field cannot strip a legitimate pending entry before sig verify (C2, M3).
    /// Value is (original_token, wallclock, sent_at).
    pending: DashMap<(Hash, Hash), (Hash, u64, Instant)>,
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
    /// Binds the token to target identity and wallclock to prevent replay (M10).
    pub fn create_ping(&self, keypair: &Keypair, target: Hash) -> PingMessage {
        let token = crypto_hash(&rand::random::<[u8; 32]>());
        let wallclock = now_ms();
        let sign_payload = hashv(&[
            b"ping",
            token.as_bytes(),
            target.as_bytes(),
            &wallclock.to_le_bytes(),
        ]);
        let sig = keypair.sign(sign_payload.as_bytes());
        let token_hash = crypto_hash(token.as_bytes());
        self.pending
            .insert((target, token_hash), (token, wallclock, Instant::now()));
        PingMessage {
            from: keypair.address(),
            token,
            target,
            wallclock,
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
    /// 1. Look up pending entry by (pong.from, pong.token_hash) without removing it (C2).
    /// 2. Check TTL.
    /// 3. Verify the pong signature against the peer's public key.
    /// 4. Only remove the pending entry on success.
    pub fn verify_pong(&self, pong: &PongMessage, pubkey: &PublicKey) -> bool {
        let key = (pong.from, pong.token_hash);

        // Read first — do NOT remove before verifying (C2 fix).
        let ttl_ok = match self.pending.get(&key) {
            Some(entry) => {
                if entry.2.elapsed() >= self.ttl {
                    metrics::counter!("nusantara_gossip_pong_expired_total").increment(1);
                    return false;
                }
                true
            }
            None => {
                metrics::counter!("nusantara_gossip_pong_unsolicited_total").increment(1);
                return false;
            }
        };

        if !ttl_ok {
            return false;
        }

        // Verify signature before touching the pending map.
        let sign_data = hashv(&[b"pong", pong.token_hash.as_bytes()]);
        if pong.signature.verify(pubkey, sign_data.as_bytes()).is_err() {
            metrics::counter!("nusantara_gossip_pong_verification_failed_total").increment(1);
            return false;
        }

        // Signature verified — now remove to prevent replay.
        self.pending.remove(&key);
        true
    }

    /// Verify a ping message signature against the sender's public key.
    /// The signature covers hashv(&[b"ping", token, target, wallclock]) (M10).
    pub fn verify_ping(ping: &PingMessage, pubkey: &PublicKey) -> bool {
        let sign_payload = hashv(&[
            b"ping",
            ping.token.as_bytes(),
            ping.target.as_bytes(),
            &ping.wallclock.to_le_bytes(),
        ]);
        if ping
            .signature
            .verify(pubkey, sign_payload.as_bytes())
            .is_err()
        {
            metrics::counter!("nusantara_gossip_ping_invalid_signature_total").increment(1);
            return false;
        }
        true
    }

    pub fn purge_expired(&self) {
        self.verified
            .retain(|_, instant| instant.elapsed() < self.ttl);
        self.purge_expired_pending();
    }

    pub fn purge_expired_pending(&self) {
        self.pending
            .retain(|_, (_, _, sent_at)| sent_at.elapsed() < self.ttl);
    }

    pub fn len(&self) -> usize {
        self.verified.len()
    }

    pub fn is_empty(&self) -> bool {
        self.verified.is_empty()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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
        assert_eq!(ping.target, target);

        let pong = PingCache::create_pong(&kp2, &ping);
        assert_eq!(pong.from, kp2.address());

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

        assert!(!cache.verify_pong(&pong, kp3.public_key()));
    }

    #[test]
    fn unsolicited_pong_rejected() {
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();
        let cache = PingCache::new(60_000);

        let fake_token = crypto_hash(b"fake_token");
        let fake_ping = PingMessage {
            from: kp1.address(),
            token: fake_token,
            target: kp2.address(),
            wallclock: now_ms(),
            signature: {
                let payload = hashv(&[
                    b"ping",
                    fake_token.as_bytes(),
                    kp2.address().as_bytes(),
                    &now_ms().to_le_bytes(),
                ]);
                kp1.sign(payload.as_bytes())
            },
        };
        let pong = PingCache::create_pong(&kp2, &fake_ping);

        assert!(!cache.verify_pong(&pong, kp2.public_key()));
    }

    #[test]
    fn forged_from_does_not_strip_pending_entry() {
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();
        let attacker_kp = Keypair::generate();
        let cache = PingCache::new(60_000);

        let ping = cache.create_ping(&kp1, kp2.address());
        let token_hash = crypto_hash(ping.token.as_bytes());

        // Attacker forges a pong with kp2's identity but wrong token_hash to probe the pending map.
        let sign_data = hashv(&[b"pong", token_hash.as_bytes()]);
        let forged_pong = PongMessage {
            from: kp2.address(),
            token_hash: crypto_hash(b"wrong_token"),
            signature: attacker_kp.sign(sign_data.as_bytes()),
        };

        // Forged pong should be rejected (key mismatch → unsolicited).
        assert!(!cache.verify_pong(&forged_pong, attacker_kp.public_key()));

        // The real pending entry must still be present.
        let real_pong = PingCache::create_pong(&kp2, &ping);
        assert!(cache.verify_pong(&real_pong, kp2.public_key()));
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
