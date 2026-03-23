use std::time::{Duration, Instant};

use serde::Serialize;

use super::tracker::TrackingResult;

#[derive(Debug, Serialize)]
pub struct BenchReport {
    pub submitted: usize,
    pub confirmed: usize,
    pub failed: usize,
    pub timed_out: usize,
    pub submit_duration_ms: u64,
    pub submit_tps: f64,
    pub confirmed_tps: f64,
    pub latency_min_ms: u64,
    pub latency_max_ms: u64,
    pub latency_mean_ms: u64,
    pub latency_p50_ms: u64,
    pub latency_p95_ms: u64,
    pub latency_p99_ms: u64,
}

impl BenchReport {
    pub fn compute(
        submitted: usize,
        submit_start: Instant,
        submit_end: Instant,
        tracking: &TrackingResult,
    ) -> Self {
        let submit_duration = submit_end.duration_since(submit_start);
        let submit_tps = if submit_duration.as_secs_f64() > 0.0 {
            submitted as f64 / submit_duration.as_secs_f64()
        } else {
            0.0
        };

        let mut latencies: Vec<Duration> = tracking
            .confirmed
            .iter()
            .map(|c| c.latency)
            .collect();
        latencies.sort();

        let (min, max, mean, p50, p95, p99) = if latencies.is_empty() {
            (
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
            )
        } else {
            let total: Duration = latencies.iter().sum();
            let mean = total / latencies.len() as u32;
            let min = latencies[0];
            let max = *latencies.last().unwrap();
            let p50 = latencies[latencies.len() / 2];
            let p95 = latencies[(latencies.len() as f64 * 0.95) as usize];
            let p99 = latencies[((latencies.len() as f64 * 0.99) as usize).min(latencies.len() - 1)];
            (min, max, mean, p50, p95, p99)
        };

        // confirmed_tps: confirmed count / time from first submit to last confirm
        let total_confirm_duration = if !tracking.confirmed.is_empty() {
            let last_confirm = tracking
                .confirmed
                .iter()
                .map(|c| c.confirm_time)
                .max()
                .unwrap();
            last_confirm.duration_since(submit_start)
        } else {
            Duration::ZERO
        };
        let confirmed_tps = if total_confirm_duration.as_secs_f64() > 0.0 {
            tracking.confirmed.len() as f64 / total_confirm_duration.as_secs_f64()
        } else {
            0.0
        };

        Self {
            submitted,
            confirmed: tracking.confirmed.len(),
            failed: tracking.failed.len(),
            timed_out: tracking.timed_out.len(),
            submit_duration_ms: submit_duration.as_millis() as u64,
            submit_tps,
            confirmed_tps,
            latency_min_ms: min.as_millis() as u64,
            latency_max_ms: max.as_millis() as u64,
            latency_mean_ms: mean.as_millis() as u64,
            latency_p50_ms: p50.as_millis() as u64,
            latency_p95_ms: p95.as_millis() as u64,
            latency_p99_ms: p99.as_millis() as u64,
        }
    }

    pub fn print_human(&self) {
        println!("=== TPS Benchmark Report ===");
        println!();
        println!("Transactions:");
        println!("  Submitted:  {}", self.submitted);
        println!("  Confirmed:  {}", self.confirmed);
        println!("  Failed:     {}", self.failed);
        println!("  Timed out:  {}", self.timed_out);
        println!();
        println!("Throughput:");
        println!("  Submit TPS:    {:.1}", self.submit_tps);
        println!("  Confirmed TPS: {:.1}", self.confirmed_tps);
        println!("  Submit time:   {} ms", self.submit_duration_ms);
        println!();
        println!("Latency (confirmed):");
        println!("  Min:  {} ms", self.latency_min_ms);
        println!("  Mean: {} ms", self.latency_mean_ms);
        println!("  P50:  {} ms", self.latency_p50_ms);
        println!("  P95:  {} ms", self.latency_p95_ms);
        println!("  P99:  {} ms", self.latency_p99_ms);
        println!("  Max:  {} ms", self.latency_max_ms);
    }

    pub fn print_json(&self) {
        println!(
            "{}",
            serde_json::to_string_pretty(self).expect("report serialization")
        );
    }
}
