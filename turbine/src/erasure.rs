use reed_solomon_erasure::galois_8::ReedSolomon;

use crate::error::TurbineError;

/// Reed-Solomon erasure coder with a cached `ReedSolomon` instance.
///
/// `ReedSolomon` construction involves Galois-field table initialization
/// which is non-trivial. Caching the instance in the struct avoids
/// rebuilding the tables on every `encode` / `recover` call.
///
/// `ReedSolomon` from `reed-solomon-erasure 6` is `Send + Sync`.
pub struct ErasureCoder {
    data_shards: usize,
    parity_shards: usize,
    rs: ReedSolomon,
}

impl ErasureCoder {
    pub fn new(data_shards: usize, parity_shards: usize) -> Result<Self, TurbineError> {
        let rs = ReedSolomon::new(data_shards, parity_shards)
            .map_err(|e| TurbineError::ErasureCoding(e.to_string()))?;
        Ok(Self {
            data_shards,
            parity_shards,
            rs,
        })
    }

    /// Create from a FEC rate percentage. E.g. 33% means ~33% parity shreds.
    pub fn from_fec_rate(
        data_shards: usize,
        fec_rate_percent: u32,
    ) -> Result<Self, TurbineError> {
        let parity_shards =
            (data_shards as u64 * fec_rate_percent as u64 / 100).max(1) as usize;
        Self::new(data_shards, parity_shards)
    }

    pub fn data_shards(&self) -> usize {
        self.data_shards
    }

    pub fn parity_shards(&self) -> usize {
        self.parity_shards
    }

    /// Encode: takes `data_shards` shards (all same length), returns `parity_shards` parity shards.
    pub fn encode(&self, data: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, TurbineError> {
        if data.len() != self.data_shards {
            return Err(TurbineError::ErasureCoding(format!(
                "expected {} data shards, got {}",
                self.data_shards,
                data.len()
            )));
        }
        if data.is_empty() {
            return Ok(Vec::new());
        }

        let shard_len = data[0].len();
        let mut all_shards: Vec<Vec<u8>> = data.to_vec();
        for _ in 0..self.parity_shards {
            all_shards.push(vec![0u8; shard_len]);
        }

        self.rs
            .encode(&mut all_shards)
            .map_err(|e| TurbineError::ErasureCoding(e.to_string()))?;

        Ok(all_shards[self.data_shards..].to_vec())
    }

    /// Recover missing shards. Input is data + parity shards with `None` for missing ones.
    pub fn recover(&self, shards: &mut [Option<Vec<u8>>]) -> Result<(), TurbineError> {
        let total = self.data_shards + self.parity_shards;
        if shards.len() != total {
            return Err(TurbineError::ErasureCoding(format!(
                "expected {} total shards, got {}",
                total,
                shards.len()
            )));
        }

        let present = shards.iter().filter(|s| s.is_some()).count();
        if present < self.data_shards {
            return Err(TurbineError::InsufficientShreds {
                have: present,
                need: self.data_shards,
            });
        }

        self.rs
            .reconstruct(shards)
            .map_err(|e| TurbineError::ErasureCoding(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_and_recover() {
        let ec = ErasureCoder::new(4, 2).unwrap();
        let data: Vec<Vec<u8>> = (0..4).map(|i| vec![i as u8; 64]).collect();

        let parity = ec.encode(&data).unwrap();
        assert_eq!(parity.len(), 2);

        // Simulate losing 2 data shards
        let mut shards: Vec<Option<Vec<u8>>> = data.iter().map(|d| Some(d.clone())).collect();
        shards.extend(parity.iter().map(|p| Some(p.clone())));

        shards[0] = None; // lose shard 0
        shards[2] = None; // lose shard 2

        ec.recover(&mut shards).unwrap();

        // Verify recovery
        assert_eq!(shards[0].as_ref().unwrap(), &data[0]);
        assert_eq!(shards[2].as_ref().unwrap(), &data[2]);
    }

    #[test]
    fn too_many_losses_fails() {
        let ec = ErasureCoder::new(4, 2).unwrap();
        let data: Vec<Vec<u8>> = (0..4).map(|i| vec![i as u8; 32]).collect();
        let parity = ec.encode(&data).unwrap();

        let mut shards: Vec<Option<Vec<u8>>> = data.iter().map(|d| Some(d.clone())).collect();
        shards.extend(parity.iter().map(|p| Some(p.clone())));

        // Lose 3 shards (more than parity count)
        shards[0] = None;
        shards[1] = None;
        shards[2] = None;

        let result = ec.recover(&mut shards);
        assert!(result.is_err());
    }

    #[test]
    fn from_fec_rate() {
        let ec = ErasureCoder::from_fec_rate(32, 33).unwrap();
        assert_eq!(ec.data_shards(), 32);
        assert_eq!(ec.parity_shards(), 10); // 32 * 33 / 100 = 10.56 -> 10
    }

    #[test]
    fn no_losses_noop() {
        let ec = ErasureCoder::new(3, 2).unwrap();
        let data: Vec<Vec<u8>> = (0..3).map(|i| vec![i as u8; 16]).collect();
        let parity = ec.encode(&data).unwrap();

        let mut shards: Vec<Option<Vec<u8>>> = data.iter().map(|d| Some(d.clone())).collect();
        shards.extend(parity.iter().map(|p| Some(p.clone())));

        ec.recover(&mut shards).unwrap();
        for (i, d) in data.iter().enumerate() {
            assert_eq!(shards[i].as_ref().unwrap(), d);
        }
    }

    #[test]
    fn rs_instance_cached_encode_is_deterministic() {
        let ec = ErasureCoder::new(4, 2).unwrap();
        let data: Vec<Vec<u8>> = (0..4).map(|i| vec![i as u8; 32]).collect();
        let p1 = ec.encode(&data).unwrap();
        let p2 = ec.encode(&data).unwrap();
        assert_eq!(p1, p2);
    }
}
