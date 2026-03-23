use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct SlotClock {
    genesis_creation_time_ms: i64,
    slot_duration_ms: u64,
}

impl SlotClock {
    pub fn new(genesis_creation_time_secs: i64, slot_duration_ms: u64) -> Self {
        Self {
            genesis_creation_time_ms: genesis_creation_time_secs * 1000,
            slot_duration_ms,
        }
    }

    pub fn current_slot(&self) -> u64 {
        let now_ms = Self::now_ms();
        let elapsed_ms = now_ms - self.genesis_creation_time_ms;
        if elapsed_ms < 0 {
            return 0;
        }
        elapsed_ms as u64 / self.slot_duration_ms
    }

    pub async fn wait_for_slot(&self, target_slot: u64) {
        let Some(offset) = target_slot.checked_mul(self.slot_duration_ms) else {
            return; // overflow — slot too large, skip waiting
        };
        let target_time_ms = self.genesis_creation_time_ms.saturating_add(offset as i64);
        let now_ms = Self::now_ms();
        let wait_ms = target_time_ms - now_ms;
        if wait_ms > 0 {
            tokio::time::sleep(Duration::from_millis(wait_ms as u64)).await;
        }
    }

    #[allow(dead_code)]
    pub async fn wait_for_next_slot(&self, current_slot: u64) {
        self.wait_for_slot(current_slot + 1).await;
    }

    /// Compute how long from now until the slot deadline (slot start + timeout_ms).
    /// Returns Duration::ZERO if deadline has already passed.
    #[allow(dead_code)]
    pub fn slot_deadline(&self, slot: u64, timeout_ms: u64) -> Duration {
        let Some(offset) = slot.checked_mul(self.slot_duration_ms) else {
            return Duration::ZERO;
        };
        let slot_start_ms = self.genesis_creation_time_ms.saturating_add(offset as i64);
        let deadline_ms = slot_start_ms.saturating_add(timeout_ms as i64);
        let now_ms = Self::now_ms();
        let wait = deadline_ms - now_ms;
        if wait > 0 {
            Duration::from_millis(wait as u64)
        } else {
            Duration::ZERO
        }
    }

    fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_millis() as i64
    }
}
