use std::num::NonZeroU64;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct SlotClock {
    genesis_creation_time_ms: i64,
    slot_duration_ms: NonZeroU64,
}

impl SlotClock {
    /// `slot_duration_ms` must be non-zero; a zero duration would cause divide-by-zero
    /// in `current_slot` and infinite busy-loops in `wait_for_slot`.
    pub fn new(genesis_creation_time_secs: i64, slot_duration_ms: NonZeroU64) -> Self {
        Self {
            // saturating_mul so an extreme genesis timestamp doesn't wrap to a
            // negative value and produce incorrect slot numbers.
            genesis_creation_time_ms: genesis_creation_time_secs.saturating_mul(1000),
            slot_duration_ms,
        }
    }

    pub fn current_slot(&self) -> u64 {
        let now_ms = Self::now_ms();
        let elapsed_ms = now_ms - self.genesis_creation_time_ms;
        if elapsed_ms < 0 {
            return 0;
        }
        elapsed_ms as u64 / self.slot_duration_ms.get()
    }

    pub async fn wait_for_slot(&self, target_slot: u64) {
        let Some(offset) = target_slot.checked_mul(self.slot_duration_ms.get()) else {
            return; // overflow — slot too large, skip waiting
        };
        // Guard against u64 → i64 cast wrapping negative (slot ~292 billion).
        // At that scale the slot is astronomically far in the future; just return.
        if offset > i64::MAX as u64 {
            return;
        }
        let target_time_ms = self.genesis_creation_time_ms.saturating_add(offset as i64);
        let now_ms = Self::now_ms();
        let wait_ms = target_time_ms - now_ms;
        if wait_ms > 0 {
            tokio::time::sleep(Duration::from_millis(wait_ms as u64)).await;
        }
    }

    fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_millis() as i64
    }
}
