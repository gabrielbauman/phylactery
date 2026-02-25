//! Shared rate limiting — in-memory sliding window, per-source.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A sliding-window rate limiter.
pub struct RateLimiter {
    limits: Mutex<HashMap<String, Vec<Instant>>>,
    window: Duration,
}

impl RateLimiter {
    pub fn new() -> Self {
        RateLimiter {
            limits: Mutex::new(HashMap::new()),
            window: Duration::from_secs(60),
        }
    }

    /// Check if a request from `source` with `max_per_minute` is allowed.
    /// Returns true if allowed, false if rate limited.
    pub fn check(&self, source: &str, max_per_minute: u32) -> bool {
        let mut limits = self.limits.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let cutoff = now - self.window;

        let entries = limits.entry(source.to_string()).or_default();

        // Remove entries outside the window
        entries.retain(|t| *t > cutoff);

        if entries.len() >= max_per_minute as usize {
            return false;
        }

        entries.push(now);
        true
    }
}

/// A deduplication cache with TTL.
pub struct DedupCache {
    cache: Mutex<HashMap<String, Instant>>,
    ttl: Duration,
}

impl DedupCache {
    pub fn new() -> Self {
        DedupCache {
            cache: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(300), // 5 minutes
        }
    }

    /// Returns true if this is a duplicate (already seen within TTL).
    pub fn is_duplicate(&self, id: &str) -> bool {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let cutoff = now - self.ttl;

        // Clean up old entries periodically (every check)
        cache.retain(|_, t| *t > cutoff);

        if cache.contains_key(id) {
            return true;
        }

        cache.insert(id.to_string(), now);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limiter_allows_under_limit() {
        let limiter = RateLimiter::new();
        assert!(limiter.check("test", 5));
        assert!(limiter.check("test", 5));
        assert!(limiter.check("test", 5));
    }

    #[test]
    fn test_rate_limiter_blocks_over_limit() {
        let limiter = RateLimiter::new();
        for _ in 0..3 {
            assert!(limiter.check("test", 3));
        }
        assert!(!limiter.check("test", 3));
    }

    #[test]
    fn test_rate_limiter_independent_sources() {
        let limiter = RateLimiter::new();
        for _ in 0..3 {
            assert!(limiter.check("a", 3));
        }
        assert!(!limiter.check("a", 3));
        // Different source should still be allowed
        assert!(limiter.check("b", 3));
    }

    #[test]
    fn test_dedup_cache_detects_duplicate() {
        let cache = DedupCache::new();
        assert!(!cache.is_duplicate("req-1"));
        assert!(cache.is_duplicate("req-1")); // duplicate
    }

    #[test]
    fn test_dedup_cache_different_ids() {
        let cache = DedupCache::new();
        assert!(!cache.is_duplicate("req-1"));
        assert!(!cache.is_duplicate("req-2")); // different ID
    }
}
