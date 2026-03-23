use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::hashv;

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct BloomFilter {
    bits: Vec<u64>,
    num_bits: u64,
    num_hashes: u32,
}

impl BloomFilter {
    /// Validate a bloom filter received from the network.
    pub fn is_valid(&self) -> bool {
        self.num_bits > 0
            && self.num_hashes > 0
            && self.num_hashes <= 16
            && self.bits.len() == self.num_bits.div_ceil(64) as usize
            && self.bits.len() <= 8192
    }

    pub fn new(num_bits: u64, num_hashes: u32) -> Self {
        let num_words = num_bits.div_ceil(64) as usize;
        Self {
            bits: vec![0u64; num_words],
            num_bits,
            num_hashes,
        }
    }

    /// Create a bloom filter sized for expected number of items with target false-positive rate.
    pub fn for_capacity(num_items: usize, fp_rate: f64) -> Self {
        let num_items = num_items.max(1) as f64;
        let num_bits = (-(num_items * fp_rate.ln()) / (2.0_f64.ln().powi(2))).ceil() as u64;
        let num_bits = num_bits.max(64);
        let num_hashes =
            ((num_bits as f64 / num_items) * 2.0_f64.ln()).ceil() as u32;
        let num_hashes = num_hashes.clamp(1, 16);
        Self::new(num_bits, num_hashes)
    }

    pub fn add(&mut self, key: &[u8]) {
        for i in 0..self.num_hashes {
            let bit_index = self.hash_index(key, i);
            let word = (bit_index / 64) as usize;
            let bit = bit_index % 64;
            if word < self.bits.len() {
                self.bits[word] |= 1u64 << bit;
            }
        }
    }

    pub fn contains(&self, key: &[u8]) -> bool {
        for i in 0..self.num_hashes {
            let bit_index = self.hash_index(key, i);
            let word = (bit_index / 64) as usize;
            let bit = bit_index % 64;
            if word >= self.bits.len() || (self.bits[word] & (1u64 << bit)) == 0 {
                return false;
            }
        }
        true
    }

    fn hash_index(&self, key: &[u8], hash_num: u32) -> u64 {
        if self.num_bits == 0 {
            return 0;
        }
        let h = hashv(&[key, &hash_num.to_le_bytes()]);
        let bytes = h.as_bytes();
        let val = u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]);
        val % self.num_bits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_contains() {
        let mut bloom = BloomFilter::new(1024, 3);
        bloom.add(b"hello");
        bloom.add(b"world");
        assert!(bloom.contains(b"hello"));
        assert!(bloom.contains(b"world"));
        assert!(!bloom.contains(b"missing"));
    }

    #[test]
    fn for_capacity() {
        let bloom = BloomFilter::for_capacity(100, 0.01);
        assert!(bloom.num_bits >= 64);
        assert!(bloom.num_hashes >= 1);
    }

    #[test]
    fn low_false_positive_rate() {
        let mut bloom = BloomFilter::for_capacity(100, 0.01);
        for i in 0u32..100 {
            bloom.add(&i.to_le_bytes());
        }
        // All inserted items must be found
        for i in 0u32..100 {
            assert!(bloom.contains(&i.to_le_bytes()));
        }
        // Count false positives from non-inserted items
        let mut fp_count = 0;
        for i in 1000u32..2000 {
            if bloom.contains(&i.to_le_bytes()) {
                fp_count += 1;
            }
        }
        // Allow up to 5% false positives (generous for small test)
        assert!(fp_count < 50, "too many false positives: {fp_count}");
    }

    #[test]
    fn borsh_roundtrip() {
        let mut bloom = BloomFilter::new(256, 2);
        bloom.add(b"test");
        let bytes = borsh::to_vec(&bloom).unwrap();
        let decoded: BloomFilter = borsh::from_slice(&bytes).unwrap();
        assert_eq!(bloom, decoded);
        assert!(decoded.contains(b"test"));
    }

    #[test]
    fn empty_bloom_contains_nothing() {
        let bloom = BloomFilter::new(256, 3);
        assert!(!bloom.contains(b"anything"));
    }

    #[test]
    fn zero_num_bits_does_not_panic() {
        let bloom = BloomFilter {
            bits: vec![],
            num_bits: 0,
            num_hashes: 3,
        };
        // Must not panic (division by zero) — just returns false
        assert!(!bloom.contains(b"anything"));
    }

    #[test]
    fn is_valid_rejects_zero_bits() {
        let bloom = BloomFilter {
            bits: vec![],
            num_bits: 0,
            num_hashes: 3,
        };
        assert!(!bloom.is_valid());
    }

    #[test]
    fn is_valid_rejects_mismatched_dimensions() {
        // num_bits=128 requires 2 words, but we only provide 1
        let bloom = BloomFilter {
            bits: vec![0u64; 1],
            num_bits: 128,
            num_hashes: 3,
        };
        assert!(!bloom.is_valid());
    }

    #[test]
    fn is_valid_accepts_correct_filter() {
        let bloom = BloomFilter::new(256, 4);
        assert!(bloom.is_valid());
    }
}
