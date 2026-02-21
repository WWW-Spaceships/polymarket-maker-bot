//! Rolling price buffer and delta (bps) computation.

use std::collections::VecDeque;

/// Computes price deltas over rolling time windows.
pub struct DeltaComputer {
    /// (timestamp_secs, price) ring buffer, oldest first.
    buffer: VecDeque<(f64, f64)>,
    /// Maximum age in seconds to retain entries.
    max_age_secs: f64,
}

impl DeltaComputer {
    pub fn new(max_age_secs: f64) -> Self {
        Self {
            buffer: VecDeque::with_capacity(2048),
            max_age_secs,
        }
    }

    /// Add a new price tick and prune old entries.
    pub fn add_tick(&mut self, timestamp: f64, price: f64) {
        self.buffer.push_back((timestamp, price));
        self.prune(timestamp);
    }

    /// Remove entries older than max_age from the latest timestamp.
    fn prune(&mut self, now: f64) {
        let cutoff = now - self.max_age_secs;
        while let Some(&(ts, _)) = self.buffer.front() {
            if ts < cutoff {
                self.buffer.pop_front();
            } else {
                break;
            }
        }
    }

    /// Compute price delta in basis points over the given window.
    pub fn compute_delta(&self, window_seconds: f64) -> super::types::DeltaResult {
        if self.buffer.is_empty() {
            return super::types::DeltaResult::unavailable();
        }

        let (latest_ts, latest_price) = *self.buffer.back().unwrap();
        let target_ts = latest_ts - window_seconds;

        // Find the entry closest to target_ts by scanning backward
        let mut best_idx = 0;
        let mut best_diff = f64::MAX;

        for (i, &(ts, _)) in self.buffer.iter().enumerate() {
            let diff = (ts - target_ts).abs();
            if diff < best_diff {
                best_diff = diff;
                best_idx = i;
            }
        }

        let (past_ts, past_price) = self.buffer[best_idx];

        if past_price <= 0.0 || latest_price <= 0.0 {
            return super::types::DeltaResult::unavailable();
        }

        let delta_bps = (latest_price - past_price) / past_price * 10_000.0;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let sample_age_ms = (now - latest_ts) * 1000.0;

        super::types::DeltaResult {
            delta_bps,
            available: true,
            sample_age_ms,
            window_start_ts: past_ts,
            window_end_ts: latest_ts,
        }
    }

    /// Get the latest price, if any.
    pub fn get_latest_price(&self) -> Option<f64> {
        self.buffer.back().map(|&(_, p)| p)
    }

    /// Get the latest timestamp, if any.
    pub fn get_latest_timestamp(&self) -> Option<f64> {
        self.buffer.back().map(|&(ts, _)| ts)
    }

    /// Number of entries in the buffer.
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}
