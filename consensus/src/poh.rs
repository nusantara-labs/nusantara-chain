use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_core::native_token::const_parse_u64;
use nusantara_crypto::{Hash, hash, hashv};
use tracing::instrument;

use crate::error::ConsensusError;

pub const HASHES_PER_TICK: u64 = const_parse_u64(env!("NUSA_POH_HASHES_PER_TICK"));
pub const TICKS_PER_SLOT: u64 = const_parse_u64(env!("NUSA_POH_TICKS_PER_SLOT"));
pub const TARGET_TICK_DURATION_US: u64 = const_parse_u64(env!("NUSA_POH_TARGET_TICK_DURATION_US"));

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct PohEntry {
    pub num_hashes: u64,
    pub hash: Hash,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Tick {
    pub entry: PohEntry,
    pub mixin: Option<Hash>,
}

pub struct PohRecorder {
    hash: Hash,
    num_hashes: u64,
    tick_count: u64,
    hashes_per_tick: u64,
}

impl PohRecorder {
    pub fn new(initial_hash: Hash) -> Self {
        Self::with_hashes_per_tick(initial_hash, HASHES_PER_TICK)
    }

    pub fn with_hashes_per_tick(initial_hash: Hash, hashes_per_tick: u64) -> Self {
        Self {
            hash: initial_hash,
            num_hashes: 0,
            tick_count: 0,
            hashes_per_tick,
        }
    }

    pub fn current_hash(&self) -> Hash {
        self.hash
    }

    pub fn tick_count(&self) -> u64 {
        self.tick_count
    }

    /// Core grinding loop: hash = sha3_512(hash) repeated `count` times.
    #[instrument(skip(self), level = "trace")]
    pub fn hash_iterations(&mut self, count: u64) {
        for _ in 0..count {
            self.hash = hash(self.hash.as_bytes());
            self.num_hashes += 1;
        }
        metrics::counter!("nusantara_poh_hash_iterations_total").increment(count);
    }

    /// Mix in a transaction hash: hash = sha3_512(hash || tx_hash).
    #[instrument(skip(self, tx_hash), level = "trace")]
    pub fn record(&mut self, tx_hash: &Hash) -> PohEntry {
        self.hash = hashv(&[self.hash.as_bytes(), tx_hash.as_bytes()]);
        self.num_hashes += 1;
        metrics::counter!("nusantara_poh_records_total").increment(1);
        PohEntry {
            num_hashes: self.num_hashes,
            hash: self.hash,
        }
    }

    /// Emit a tick after HASHES_PER_TICK iterations.
    #[instrument(skip(self), level = "debug")]
    pub fn tick(&mut self) -> Tick {
        self.hash_iterations(
            self.hashes_per_tick
                .saturating_sub(self.num_hashes % self.hashes_per_tick),
        );
        let entry = PohEntry {
            num_hashes: self.num_hashes,
            hash: self.hash,
        };
        self.tick_count += 1;
        metrics::counter!("nusantara_poh_ticks_total").increment(1);
        Tick { entry, mixin: None }
    }

    /// Produce a complete slot worth of ticks.
    #[instrument(skip(self), level = "debug")]
    pub fn produce_slot(&mut self) -> Vec<Tick> {
        let mut ticks = Vec::with_capacity(TICKS_PER_SLOT as usize);
        for _ in 0..TICKS_PER_SLOT {
            ticks.push(self.tick());
        }
        metrics::counter!("nusantara_poh_slots_produced_total").increment(1);
        ticks
    }

    /// Reset the hash counter (used at slot boundaries).
    pub fn reset(&mut self, initial_hash: Hash) {
        self.hash = initial_hash;
        self.num_hashes = 0;
        self.tick_count = 0;
    }
}

/// CPU verification of a PoH entry chain.
/// Each entry's hash must be reproducible from the previous hash
/// by iterating sha3_512 for (entry.num_hashes - prev_num_hashes) times.
#[instrument(skip(initial_hash, entries), level = "debug")]
pub fn verify_poh_entries(initial_hash: &Hash, entries: &[PohEntry]) -> bool {
    let mut current = *initial_hash;
    let mut prev_num = 0u64;

    for entry in entries {
        let iterations = entry.num_hashes.saturating_sub(prev_num);
        for _ in 0..iterations {
            current = hash(current.as_bytes());
        }
        if current != entry.hash {
            return false;
        }
        prev_num = entry.num_hashes;
    }
    true
}

/// Verify a PoH chain with optional transaction mixins.
/// `entries` is a sequence of (num_hashes_delta, optional_mixin, expected_hash).
#[instrument(skip(initial_hash, entries), level = "debug")]
pub fn verify_poh_chain(
    initial_hash: &Hash,
    entries: &[(u64, Option<Hash>, Hash)],
) -> Result<(), ConsensusError> {
    let mut current = *initial_hash;

    for (i, (num_hashes, mixin, expected)) in entries.iter().enumerate() {
        // Hash iterations before the mixin (if any)
        let pre_mixin_hashes = if mixin.is_some() {
            num_hashes.saturating_sub(1)
        } else {
            *num_hashes
        };

        for _ in 0..pre_mixin_hashes {
            current = hash(current.as_bytes());
        }

        if let Some(mix) = mixin {
            current = hashv(&[current.as_bytes(), mix.as_bytes()]);
        }

        if current != *expected {
            return Err(ConsensusError::PohVerificationFailed { index: i });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_values() {
        assert_eq!(HASHES_PER_TICK, 12_500);
        assert_eq!(TICKS_PER_SLOT, 64);
        assert_eq!(TARGET_TICK_DURATION_US, 14_062);
    }

    #[test]
    fn poh_recorder_basic() {
        let init = hash(b"genesis");
        let mut recorder = PohRecorder::new(init);
        assert_eq!(recorder.current_hash(), init);

        recorder.hash_iterations(10);
        assert_ne!(recorder.current_hash(), init);
    }

    #[test]
    fn poh_record_mixin() {
        let init = hash(b"genesis");
        let mut recorder = PohRecorder::new(init);
        let tx_hash = hash(b"transaction");
        let entry = recorder.record(&tx_hash);
        assert_ne!(entry.hash, init);
        assert_eq!(entry.num_hashes, 1);
    }

    #[test]
    fn poh_tick_advances() {
        let init = hash(b"genesis");
        let mut recorder = PohRecorder::new(init);
        let tick = recorder.tick();
        assert_eq!(tick.entry.num_hashes, HASHES_PER_TICK);
        assert_eq!(recorder.tick_count(), 1);
    }

    #[test]
    fn poh_produce_slot() {
        let init = hash(b"genesis");
        let mut recorder = PohRecorder::new(init);
        let ticks = recorder.produce_slot();
        assert_eq!(ticks.len(), TICKS_PER_SLOT as usize);
        assert_eq!(
            ticks.last().unwrap().entry.num_hashes,
            HASHES_PER_TICK * TICKS_PER_SLOT
        );
    }

    #[test]
    fn poh_verify_entries() {
        let init = hash(b"genesis");
        let mut recorder = PohRecorder::new(init);

        let mut entries = Vec::new();
        for _ in 0..3 {
            let tick = recorder.tick();
            entries.push(tick.entry);
        }

        assert!(verify_poh_entries(&init, &entries));
    }

    #[test]
    fn poh_verify_detects_tamper() {
        let init = hash(b"genesis");
        let mut recorder = PohRecorder::new(init);

        let mut entries = Vec::new();
        for _ in 0..3 {
            let tick = recorder.tick();
            entries.push(tick.entry);
        }

        // Tamper with middle entry
        entries[1].hash = hash(b"tampered");
        assert!(!verify_poh_entries(&init, &entries));
    }

    #[test]
    fn poh_deterministic() {
        let init = hash(b"genesis");
        let mut r1 = PohRecorder::new(init);
        let mut r2 = PohRecorder::new(init);

        r1.hash_iterations(100);
        r2.hash_iterations(100);

        assert_eq!(r1.current_hash(), r2.current_hash());
    }

    #[test]
    fn poh_with_hashes_per_tick() {
        let init = hash(b"genesis");
        let mut recorder = PohRecorder::with_hashes_per_tick(init, 1);
        let tick = recorder.tick();
        assert_eq!(tick.entry.num_hashes, 1);
        assert_eq!(recorder.tick_count(), 1);

        let ticks = recorder.produce_slot();
        assert_eq!(ticks.len(), TICKS_PER_SLOT as usize);
        // After produce_slot: 1 (from first tick) + TICKS_PER_SLOT * 1 hashes
        assert_eq!(ticks.last().unwrap().entry.num_hashes, 1 + TICKS_PER_SLOT);
    }

    #[test]
    fn poh_reset() {
        let init = hash(b"genesis");
        let mut recorder = PohRecorder::new(init);
        recorder.hash_iterations(100);
        let new_init = hash(b"new_genesis");
        recorder.reset(new_init);
        assert_eq!(recorder.current_hash(), new_init);
        assert_eq!(recorder.tick_count(), 0);
    }
}
