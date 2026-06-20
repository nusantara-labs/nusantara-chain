use nusantara_crypto::{Hash, hashv};

/// Stake-weighted deterministic shuffle.
///
/// Each node receives a sort key: `stake_score * 2^64 + deterministic_hash_tiebreak`.
/// The deterministic tiebreak is derived from `hashv(&[seed, identity])` so the
/// order varies per-push-round (seed changes) while remaining fully reproducible
/// given the same seed — no random component, no floating-point, no cross-arch
/// non-determinism (L2 fix).
pub fn weighted_shuffle(stakes: &[(Hash, u64)], seed: &Hash) -> Vec<usize> {
    if stakes.is_empty() {
        return Vec::new();
    }

    let total_stake: u64 = stakes.iter().map(|(_, s)| *s).sum();

    let mut weighted: Vec<(usize, u128)> = stakes
        .iter()
        .enumerate()
        .map(|(i, (identity, stake))| {
            let h = hashv(&[seed.as_bytes(), identity.as_bytes()]);
            let bytes = h.as_bytes();
            let tiebreak = u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]);

            let key = if total_stake == 0 {
                // All zero stakes: sort purely by tiebreak.
                tiebreak as u128
            } else {
                // stake_score occupies the high 64 bits; tiebreak the low 64 bits.
                // This ensures higher-stake nodes always rank above lower-stake nodes
                // regardless of tiebreak, while still producing varied orderings
                // among same-stake nodes across different seeds.
                let stake_score = (*stake as u128) * u64::MAX as u128 / (total_stake as u128);
                (stake_score << 64) | tiebreak as u128
            };

            (i, key)
        })
        .collect();

    weighted.sort_by_key(|&(_, key)| std::cmp::Reverse(key));
    weighted.into_iter().map(|(i, _)| i).collect()
}

/// Select up to `count` peers using stake-weighted shuffle (L4).
pub fn select_peers(peers: &[(Hash, u64)], seed: &Hash, count: usize) -> Vec<Hash> {
    let indices = weighted_shuffle(peers, seed);
    indices
        .into_iter()
        .take(count)
        .map(|i| peers[i].0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    #[test]
    fn deterministic_shuffle() {
        let seed = hash(b"test_seed");
        let stakes = vec![(hash(b"v1"), 500), (hash(b"v2"), 300), (hash(b"v3"), 200)];

        let s1 = weighted_shuffle(&stakes, &seed);
        let s2 = weighted_shuffle(&stakes, &seed);
        assert_eq!(s1, s2);
    }

    #[test]
    fn different_seeds_different_order() {
        let stakes = vec![
            (hash(b"v1"), 500),
            (hash(b"v2"), 500),
            (hash(b"v3"), 500),
            (hash(b"v4"), 500),
        ];

        let s1 = weighted_shuffle(&stakes, &hash(b"seed1"));
        let s2 = weighted_shuffle(&stakes, &hash(b"seed2"));
        assert_ne!(s1, s2);
    }

    #[test]
    fn empty_stakes() {
        let result = weighted_shuffle(&[], &hash(b"seed"));
        assert!(result.is_empty());
    }

    #[test]
    fn all_indices_present() {
        let stakes = vec![(hash(b"v1"), 100), (hash(b"v2"), 200), (hash(b"v3"), 300)];
        let result = weighted_shuffle(&stakes, &hash(b"seed"));
        assert_eq!(result.len(), 3);
        let mut sorted = result.clone();
        sorted.sort();
        assert_eq!(sorted, vec![0, 1, 2]);
    }

    #[test]
    fn higher_stake_tends_to_be_first() {
        let stakes = vec![(hash(b"small"), 1), (hash(b"huge"), 999_999)];

        let mut high_first = 0;
        for i in 0u64..100 {
            let seed = hash(&i.to_le_bytes());
            let result = weighted_shuffle(&stakes, &seed);
            if result[0] == 1 {
                high_first += 1;
            }
        }
        assert!(
            high_first > 80,
            "high stake first only {high_first}/100 times"
        );
    }

    #[test]
    fn all_zero_stakes_produces_valid_shuffle() {
        let stakes = vec![(hash(b"a"), 0), (hash(b"b"), 0), (hash(b"c"), 0)];
        let result = weighted_shuffle(&stakes, &hash(b"seed"));
        assert_eq!(result.len(), 3);
        let mut sorted = result.clone();
        sorted.sort();
        assert_eq!(sorted, vec![0, 1, 2]);
    }

    #[test]
    fn select_peers_subset() {
        let peers: Vec<(Hash, u64)> = (0..10)
            .map(|i| (hash(&(i as u64).to_le_bytes()), 100))
            .collect();
        let selected = select_peers(&peers, &hash(b"seed"), 3);
        assert_eq!(selected.len(), 3);
    }

    #[test]
    fn select_peers_deterministic() {
        let peers: Vec<(Hash, u64)> = (0..5)
            .map(|i| (hash(&(i as u64).to_le_bytes()), 100 * (i + 1) as u64))
            .collect();
        let s1 = select_peers(&peers, &hash(b"seed"), 3);
        let s2 = select_peers(&peers, &hash(b"seed"), 3);
        assert_eq!(s1, s2);
    }
}
