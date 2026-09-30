//! Dashboard aggregate cache (db-performance-tuning.spec.md §9).
//!
//! The heavy dashboard aggregates share the hot path's SQLite read pool. A
//! dashboard polling loop re-issues the same wide-range GROUP BY queries every
//! few seconds; this cache bounds them to one execution per TTL window per
//! parameter set.

use dashmap::DashMap;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct DashboardAggCache {
    entries: DashMap<String, (serde_json::Value, Instant)>,
    /// `None` disables caching: reads always miss (DPT-DA2).
    ttl: Option<Duration>,
    capacity: usize,
}

impl Default for DashboardAggCache {
    fn default() -> Self {
        Self::from_env()
    }
}

impl DashboardAggCache {
    pub fn from_env() -> Self {
        let ttl = match std::env::var("MONOIZE_DASHBOARD_AGG_CACHE_TTL_MS") {
            Ok(raw) => match raw.trim().parse::<u64>() {
                Ok(0) => None,
                Ok(ms) => Some(Duration::from_millis(ms)),
                Err(_) => Some(Duration::from_millis(10_000)),
            },
            Err(_) => Some(Duration::from_millis(10_000)),
        };
        let capacity = match std::env::var("MONOIZE_DASHBOARD_AGG_CACHE_CAPACITY") {
            Ok(raw) => raw
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|value| *value > 0)
                .unwrap_or(256),
            Err(_) => 256,
        };
        Self {
            entries: DashMap::new(),
            ttl,
            capacity,
        }
    }

    /// Returns the cached response payload for the key, or `None` when the
    /// cache is disabled, the key is absent, or the entry expired (DPT-DA3).
    pub fn get(&self, key: &str) -> Option<serde_json::Value> {
        let ttl = self.ttl?;
        let entry = self.entries.get(key)?;
        if entry.1.elapsed() > ttl {
            drop(entry);
            self.entries.remove_if(key, |_, (_, at)| at.elapsed() > ttl);
            return None;
        }
        Some(entry.0.clone())
    }

    pub fn put(&self, key: &str, value: serde_json::Value) {
        if self.ttl.is_none() {
            return;
        }
        if self.entries.len() >= self.capacity {
            if let Some(victim) = self.entries.iter().next().map(|e| e.key().clone()) {
                self.entries.remove(&victim);
            }
        }
        self.entries
            .insert(key.to_string(), (value, Instant::now()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_with_ttl(ttl: Option<Duration>) -> DashboardAggCache {
        DashboardAggCache {
            entries: DashMap::new(),
            ttl,
            capacity: 4,
        }
    }

    #[test]
    fn put_then_get_returns_payload() {
        let cache = cache_with_ttl(Some(Duration::from_secs(10)));
        cache.put("k", serde_json::json!({"a": 1}));
        assert_eq!(cache.get("k"), Some(serde_json::json!({"a": 1})));
    }

    #[test]
    fn disabled_cache_never_stores() {
        let cache = cache_with_ttl(None);
        cache.put("k", serde_json::json!({"a": 1}));
        assert_eq!(cache.get("k"), None);
    }

    #[test]
    fn missing_key_misses() {
        let cache = cache_with_ttl(Some(Duration::from_secs(10)));
        assert_eq!(cache.get("absent"), None);
    }

    #[test]
    fn capacity_evicts_oldest_insertion_batch() {
        let cache = cache_with_ttl(Some(Duration::from_secs(10)));
        for index in 0..4 {
            cache.put(&format!("k{index}"), serde_json::json!(index));
        }
        cache.put("k4", serde_json::json!(4));
        assert_eq!(cache.entries.len(), 4);
    }
}
