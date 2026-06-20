use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::hashv;

/// Maximum number of bits in a bloom filter received from the network.
/// 8192 words × 64 bits/word = 524 288 bits. Caps attacker-controlled
/// Vec allocation before it happens (C5).
const MAX_BLOOM_BITS: u64 = 524_288;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BloomFilter {
    bits: Vec<u64>,
    num_bits: u64,
    num_hashes: u32,
}

impl BorshSerialize for BloomFilter {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        self.bits.serialize(writer)?;
        self.num_bits.serialize(writer)?;
        self.num_hashes.serialize(writer)
    }
}

impl BorshDeserialize for BloomFilter {
    fn deserialize_reader<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        // Read `bits` length prefix and validate BEFORE allocating (C5).
        let words_len = u32::deserialize_reader(reader)? as usize;
        let max_words = (MAX_BLOOM_BITS.div_ceil(64)) as usize;
        if words_len > max_words {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bloom filter too large: {words_len} words (max {max_words})"),
            ));
        }
        let mut bits = Vec::with_capacity(words_len);
        for _ in 0..words_len {
            bits.push(u64::deserialize_reader(reader)?);
        }
        let num_bits = u64::deserialize_reader(reader)?;
        let num_hashes = u32::deserialize_reader(reader)?;

        // Structural integrity: words count must match num_bits.
        let expected_words = num_bits.div_ceil(64) as usize;
        if bits.len() != expected_words {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "bloom filter dimension mismatch: {} words for {num_bits} bits (expected {expected_words})",
                    bits.len()
                ),
            ));
        }

        Ok(Self {
            bits,
            num_bits,
            num_hashes,
        })
    }
}

impl BloomFilter {
    /// Validate a bloom filter received from the network.
    /// Requires minimum density: at least 64 bits per hash function (M2).
    pub fn is_valid(&self) -> bool {
        self.num_bits > 0
            && self.num_hashes > 0
            && self.num_hashes <= 16
            && self.bits.len() == self.num_bits.div_ceil(64) as usize
            && self.bits.len() <= 8192
            && self.num_bits >= 64 * self.num_hashes as u64
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
    /// Guarantees the result satisfies `is_valid()`.
    pub fn for_capacity(num_items: usize, fp_rate: f64) -> Self {
        let num_items = num_items.max(1) as f64;
        let num_bits = (-(num_items * fp_rate.ln()) / (2.0_f64.ln().powi(2))).ceil() as u64;
        let num_bits = num_bits.max(64);
        let num_hashes = ((num_bits as f64 / num_items) * 2.0_f64.ln()).ceil() as u32;
        let num_hashes_clamped = num_hashes.clamp(1, 16);
        if num_hashes_clamped != num_hashes {
            tracing::warn!(
                requested = num_hashes,
                clamped = num_hashes_clamped,
                "bloom filter num_hashes clamped; false-positive rate may be higher than requested"
            );
        }
        // Ensure density requirement: num_bits >= 64 * num_hashes (M2 invariant).
        let num_bits = num_bits.max(64 * num_hashes_clamped as u64);
        Self::new(num_bits, num_hashes_clamped)
    }

    pub fn add(&mut self, key: &[u8]) {
        let (h1, h2) = self.km_hashes(key);
        for i in 0..self.num_hashes {
            let bit_index = h1.wrapping_add((i as u64).wrapping_mul(h2)) % self.num_bits;
            let word = (bit_index / 64) as usize;
            let bit = bit_index % 64;
            self.bits[word] |= 1u64 << bit;
        }
    }

    pub fn contains(&self, key: &[u8]) -> bool {
        if self.num_bits == 0 {
            return false;
        }
        let (h1, h2) = self.km_hashes(key);
        for i in 0..self.num_hashes {
            let bit_index = h1.wrapping_add((i as u64).wrapping_mul(h2)) % self.num_bits;
            let word = (bit_index / 64) as usize;
            let bit = bit_index % 64;
            if word >= self.bits.len() || (self.bits[word] & (1u64 << bit)) == 0 {
                return false;
            }
        }
        true
    }

    /// Kirsch-Mitzenmacher double hashing: one SHA3-512 gives h1 and h2,
    /// then bit_i = (h1 + i*h2) % num_bits. Saves one hash call per probe (M1).
    fn km_hashes(&self, key: &[u8]) -> (u64, u64) {
        let h = hashv(&[key]);
        let bytes = h.as_bytes();
        let h1 = u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]);
        let h2 = u64::from_le_bytes([
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ]);
        (h1, h2)
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
        for i in 0u32..100 {
            assert!(bloom.contains(&i.to_le_bytes()));
        }
        let mut fp_count = 0;
        for i in 1000u32..2000 {
            if bloom.contains(&i.to_le_bytes()) {
                fp_count += 1;
            }
        }
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
    fn zero_num_bits_is_invalid() {
        let bloom = BloomFilter {
            bits: vec![],
            num_bits: 0,
            num_hashes: 3,
        };
        assert!(!bloom.is_valid());
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

    #[test]
    fn is_valid_rejects_insufficient_density() {
        // num_bits=64, num_hashes=2 → need 64*2=128 bits minimum
        let bloom = BloomFilter::new(64, 2);
        assert!(!bloom.is_valid());
    }

    #[test]
    fn deserialize_rejects_oversized_bits_vec() {
        // Craft raw bytes: words_len > MAX_BLOOM_BITS/64
        let max_words = (MAX_BLOOM_BITS.div_ceil(64)) as u32;
        let oversized = max_words + 1;
        let mut crafted = Vec::new();
        crafted.extend_from_slice(&oversized.to_le_bytes()); // words length prefix
        let result = BloomFilter::deserialize(&mut crafted.as_slice());
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_rejects_dimension_mismatch() {
        // Write a valid-sized bits vec but with num_bits inconsistent with length.
        // bits = [0u64; 1], num_bits = 128 (needs 2 words), num_hashes = 1
        let mut crafted = Vec::new();
        crafted.extend_from_slice(&1u32.to_le_bytes()); // words_len = 1
        crafted.extend_from_slice(&0u64.to_le_bytes()); // one word
        crafted.extend_from_slice(&128u64.to_le_bytes()); // num_bits = 128 → needs 2 words
        crafted.extend_from_slice(&1u32.to_le_bytes()); // num_hashes
        let result = BloomFilter::deserialize(&mut crafted.as_slice());
        assert!(result.is_err());
    }
}
