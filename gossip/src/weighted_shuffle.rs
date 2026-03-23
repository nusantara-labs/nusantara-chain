use nusantara_crypto::{Hash, hashv};

/// Fixed-point scale factor for u128 integer arithmetic (10^18).
/// Provides enough precision to replace f64 while remaining deterministic.
const SCALE: u128 = 1_000_000_000_000_000_000;

/// Stake-weighted deterministic shuffle using u128 fixed-point arithmetic.
/// Returns indices sorted by stake-weighted priority derived from the seed.
///
/// Uses integer math exclusively to ensure identical ordering across all
/// CPU architectures (no floating-point non-determinism).
pub fn weighted_shuffle(stakes: &[(Hash, u64)], seed: &Hash) -> Vec<usize> {
    if stakes.is_empty() {
        return Vec::new();
    }

    let total_stake: u64 = stakes.iter().map(|(_, s)| *s).sum();
    if total_stake == 0 {
        // All zero stakes: return sequential indices
        return (0..stakes.len()).collect();
    }

    // Generate a deterministic weight for each node using u128 fixed-point
    let mut weighted: Vec<(usize, u128)> = stakes
        .iter()
        .enumerate()
        .map(|(i, (identity, stake))| {
            let h = hashv(&[seed.as_bytes(), identity.as_bytes()]);
            let bytes = h.as_bytes();
            let rand_val = u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]);

            // stake_component = stake * SCALE / total_stake
            let stake_component = (*stake as u128) * SCALE / (total_stake as u128);
            // rand_component = rand_val * (SCALE / 100) / u64::MAX
            let rand_component =
                (rand_val as u128) * (SCALE / 100) / (u64::MAX as u128);

            let weight = stake_component + rand_component;
            (i, weight)
        })
        .collect();

    weighted.sort_by(|a, b| b.1.cmp(&a.1));
    weighted.into_iter().map(|(i, _)| i).collect()
}

/// Select up to `count` peers using stake-weighted shuffle.
pub fn select_peers<T: Clone>(
    peers: &[(T, u64)],
    seed: &Hash,
    count: usize,
) -> Vec<T> {
    let stakes: Vec<(Hash, u64)> = peers
        .iter()
        .enumerate()
        .map(|(i, (_, stake))| {
            let h = hashv(&[seed.as_bytes(), &(i as u64).to_le_bytes()]);
            (h, *stake)
        })
        .collect();

    let indices = weighted_shuffle(&stakes, seed);
    indices
        .into_iter()
        .take(count)
        .map(|i| peers[i].0.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    #[test]
    fn deterministic_shuffle() {
        let seed = hash(b"test_seed");
        let stakes = vec![
            (hash(b"v1"), 500),
            (hash(b"v2"), 300),
            (hash(b"v3"), 200),
        ];

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
        // Very unlikely to be the same with 4! permutations
        assert_ne!(s1, s2);
    }

    #[test]
    fn empty_stakes() {
        let result = weighted_shuffle(&[], &hash(b"seed"));
        assert!(result.is_empty());
    }

    #[test]
    fn all_indices_present() {
        let stakes = vec![
            (hash(b"v1"), 100),
            (hash(b"v2"), 200),
            (hash(b"v3"), 300),
        ];
        let result = weighted_shuffle(&stakes, &hash(b"seed"));
        assert_eq!(result.len(), 3);
        let mut sorted = result.clone();
        sorted.sort();
        assert_eq!(sorted, vec![0, 1, 2]);
    }

    #[test]
    fn higher_stake_tends_to_be_first() {
        let stakes = vec![
            (hash(b"small"), 1),
            (hash(b"huge"), 999_999),
        ];

        // Over many seeds, the high-stake node should be first most of the time
        let mut high_first = 0;
        for i in 0u64..100 {
            let seed = hash(&i.to_le_bytes());
            let result = weighted_shuffle(&stakes, &seed);
            if result[0] == 1 {
                high_first += 1;
            }
        }
        assert!(high_first > 80, "high stake first only {high_first}/100 times");
    }

    #[test]
    fn select_peers_subset() {
        let peers: Vec<(Hash, u64)> = (0..10)
            .map(|i| (hash(&(i as u64).to_le_bytes()), 100))
            .collect();
        let selected = select_peers(&peers, &hash(b"seed"), 3);
        assert_eq!(selected.len(), 3);
    }
}
