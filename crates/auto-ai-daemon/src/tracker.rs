//! Usage tracker — per-app token/cost accounting.

use std::collections::HashMap;

use parking_lot::Mutex;

#[derive(Debug, Default, Clone)]
pub struct AppUsage {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub request_count: u64,
    /// Plan 028: cache-hit accounting (prompt tokens served from cache).
    pub total_cache_read_tokens: u64,
    /// Plan 028: prompt tokens written into the cache.
    pub total_cache_write_tokens: u64,
}

impl AppUsage {
    pub fn total_tokens(&self) -> u64 {
        self.total_input_tokens + self.total_output_tokens
    }
}

/// Thread-safe usage tracker. Records per-app token consumption.
pub struct UsageTracker {
    apps: Mutex<HashMap<String, AppUsage>>,
}

impl UsageTracker {
    pub fn new() -> Self {
        Self { apps: Mutex::new(HashMap::new()) }
    }

    /// Record usage for an app (Plan 028: cache dimensions included).
    pub fn record(&self, app: &str, input: u64, output: u64) {
        self.record_full(app, input, output, 0, 0);
    }

    /// Record usage for an app with cache dimensions.
    pub fn record_full(&self, app: &str, input: u64, output: u64, cache_read: u64, cache_write: u64) {
        let mut apps = self.apps.lock();
        let entry = apps.entry(app.to_string()).or_default();
        entry.total_input_tokens += input;
        entry.total_output_tokens += output;
        entry.total_cache_read_tokens += cache_read;
        entry.total_cache_write_tokens += cache_write;
        entry.request_count += 1;
    }

    /// Get usage for an app.
    pub fn get(&self, app: &str) -> AppUsage {
        self.apps.lock().get(app).cloned().unwrap_or_default()
    }

    /// Get all app usage as (app_name, AppUsage) pairs.
    pub fn all(&self) -> Vec<(String, AppUsage)> {
        self.apps.lock().iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
}

impl Default for UsageTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_get() {
        let tracker = UsageTracker::new();
        tracker.record("ash", 100, 50);
        tracker.record("ash", 200, 80);
        let usage = tracker.get("ash");
        assert_eq!(usage.total_input_tokens, 300);
        assert_eq!(usage.total_output_tokens, 130);
        assert_eq!(usage.request_count, 2);
    }

    #[test]
    fn record_full_accumulates_cache_dimensions() {
        // Plan 028 debt: record_full (the /v1/chat/completions path) and the
        // record() wrapper must land in the same per-app buckets.
        let tracker = UsageTracker::new();
        tracker.record_full("app", 1000, 200, 700, 250);
        tracker.record_full("app", 500, 100, 0, 0);
        tracker.record("app", 10, 5); // record() == record_full(.., 0, 0)
        let u = tracker.get("app");
        assert_eq!(u.total_input_tokens, 1510);
        assert_eq!(u.total_output_tokens, 305);
        assert_eq!(u.total_cache_read_tokens, 700);
        assert_eq!(u.total_cache_write_tokens, 250);
        assert_eq!(u.request_count, 3);
        // The /v1/usage projection reads exactly these fields.
        assert_eq!(u.total_tokens(), 1510 + 305);
    }

    #[test]
    fn unknown_app_zero() {
        let tracker = UsageTracker::new();
        let usage = tracker.get("nonexistent");
        assert_eq!(usage.total_tokens(), 0);
    }

    #[test]
    fn multiple_apps() {
        let tracker = UsageTracker::new();
        tracker.record("ash", 100, 50);
        tracker.record("forge", 5000, 1000);
        let all = tracker.all();
        assert_eq!(all.len(), 2);
    }
}
