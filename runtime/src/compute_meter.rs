use crate::error::RuntimeError;

#[derive(Debug)]
pub struct ComputeMeter {
    remaining: u64,
    limit: u64,
}

impl ComputeMeter {
    pub fn new(limit: u64) -> Self {
        Self {
            remaining: limit,
            limit,
        }
    }

    pub fn consume(&mut self, units: u64) -> Result<(), RuntimeError> {
        if units > self.remaining {
            let remaining = self.remaining;
            self.remaining = 0;
            return Err(RuntimeError::InsufficientComputeUnits {
                needed: units,
                remaining,
            });
        }
        self.remaining -= units;
        Ok(())
    }

    pub fn remaining(&self) -> u64 {
        self.remaining
    }

    pub fn consumed(&self) -> u64 {
        self.limit - self.remaining
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// Set remaining compute units directly (for WASM fuel sync).
    /// Capped at the meter's limit to prevent exceeding the original budget.
    pub fn set_remaining(&mut self, remaining: u64) {
        self.remaining = remaining.min(self.limit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_meter() {
        let meter = ComputeMeter::new(1000);
        assert_eq!(meter.remaining(), 1000);
        assert_eq!(meter.consumed(), 0);
        assert_eq!(meter.limit(), 1000);
    }

    #[test]
    fn consume_within_budget() {
        let mut meter = ComputeMeter::new(1000);
        meter.consume(400).unwrap();
        assert_eq!(meter.remaining(), 600);
        assert_eq!(meter.consumed(), 400);
    }

    #[test]
    fn consume_exact_budget() {
        let mut meter = ComputeMeter::new(500);
        meter.consume(500).unwrap();
        assert_eq!(meter.remaining(), 0);
        assert_eq!(meter.consumed(), 500);
    }

    #[test]
    fn consume_exceeds_budget() {
        let mut meter = ComputeMeter::new(100);
        let err = meter.consume(150).unwrap_err();
        assert!(matches!(
            err,
            RuntimeError::InsufficientComputeUnits {
                needed: 150,
                remaining: 100
            }
        ));
        assert_eq!(meter.remaining(), 0);
    }

    #[test]
    fn consumed_tracking() {
        let mut meter = ComputeMeter::new(1000);
        meter.consume(100).unwrap();
        meter.consume(200).unwrap();
        meter.consume(300).unwrap();
        assert_eq!(meter.consumed(), 600);
        assert_eq!(meter.remaining(), 400);
    }

    #[test]
    fn set_remaining_capped_at_limit() {
        let mut meter = ComputeMeter::new(1000);
        meter.consume(500).unwrap();
        meter.set_remaining(2000); // exceeds limit
        assert_eq!(meter.remaining(), 1000); // capped at limit
    }
}
