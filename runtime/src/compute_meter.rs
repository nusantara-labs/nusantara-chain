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
    ///
    /// The meter is monotonically non-increasing: `remaining` is capped at the
    /// current `self.remaining` so WASM fuel sync can never increase the budget
    /// mid-execution. This prevents a crafted WASM module from recovering units
    /// it has already consumed.
    pub fn set_remaining(&mut self, remaining: u64) {
        self.remaining = remaining.min(self.remaining);
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
    fn set_remaining_monotonically_non_increasing() {
        let mut meter = ComputeMeter::new(1000);
        meter.consume(500).unwrap();
        // Attempting to set a value larger than current remaining is clamped
        // to current remaining — the meter cannot be refilled mid-execution.
        meter.set_remaining(2000);
        assert_eq!(meter.remaining(), 500);
        // A lower value is accepted (WASM fuel sync reporting fewer units left).
        meter.set_remaining(300);
        assert_eq!(meter.remaining(), 300);
        // Trying to go back up is still clamped.
        meter.set_remaining(400);
        assert_eq!(meter.remaining(), 300);
    }
}
