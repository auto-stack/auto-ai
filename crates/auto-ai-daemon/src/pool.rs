//! Concurrency management — per-provider/per-model Semaphore pools.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Manages concurrency limits per provider.
/// Each provider has a Semaphore(max_concurrency).
/// Acquiring a permit blocks until a slot is free.
pub struct ConcurrencyManager {
    pools: HashMap<String, Arc<Semaphore>>,
    limits: HashMap<String, usize>,
}

impl ConcurrencyManager {
    /// Build from daemon config.
    pub fn from_config(config: &crate::config::DaemonConfig) -> Self {
        let mut pools = HashMap::new();
        let mut limits = HashMap::new();
        for (name, provider) in &config.providers {
            // max_concurrency is Option<usize> in the shared config; default
            // to a sane cap when a provider doesn't set it.
            let limit = provider.max_concurrency.unwrap_or(4);
            pools.insert(name.clone(), Arc::new(Semaphore::new(limit)));
            limits.insert(name.clone(), limit);
        }
        Self { pools, limits }
    }

    /// Acquire a concurrency permit for a provider.
    /// Returns a guard that releases on drop.
    ///
    /// Blocks indefinitely until a slot frees. For bounded waiting (so the
    /// caller can fail fast / fall back when the provider is saturated), use
    /// [`Self::acquire_with_timeout`].
    pub async fn acquire(&self, provider: &str) -> Option<tokio::sync::OwnedSemaphorePermit> {
        let sem = self.pools.get(provider)?;
        Some(sem.clone().acquire_owned().await.ok()?)
    }

    /// Like [`Self::acquire`] but gives up after `timeout`, returning `None`.
    /// This makes the "concurrency pool unavailable" path actually reachable
    /// (the bare `acquire` waits forever, so a saturated provider would just
    /// queue requests until the client's own timeout).
    pub async fn acquire_with_timeout(
        &self,
        provider: &str,
        timeout: std::time::Duration,
    ) -> Option<tokio::sync::OwnedSemaphorePermit> {
        let sem = self.pools.get(provider)?;
        match tokio::time::timeout(timeout, sem.clone().acquire_owned()).await {
            Ok(Ok(permit)) => Some(permit),
            Ok(Err(_)) => None, // semaphore closed
            Err(_) => {
                tracing::warn!(
                    "concurrency pool for '{}' saturated: acquire timed out after {:?}",
                    provider,
                    timeout
                );
                None
            }
        }
    }

    /// How many permits are currently held (in use) for a provider.
    /// (The previous `available()` returned this same value but was misnamed —
    /// `limit - available_permits()` is the *in-use* count, not free slots.)
    pub fn in_use(&self, provider: &str) -> Option<usize> {
        let sem = self.pools.get(provider)?;
        let limit = self.limits.get(provider).copied().unwrap_or(0);
        Some(limit - sem.available_permits())
    }

    /// Max concurrency for a provider.
    pub fn limit(&self, provider: &str) -> usize {
        self.limits.get(provider).copied().unwrap_or(0)
    }

    /// Status snapshot: provider → (available, max).
    pub fn status(&self) -> Vec<(String, usize, usize)> {
        self.pools.iter().map(|(name, sem)| {
            let max = self.limits.get(name).copied().unwrap_or(0);
            (name.clone(), sem.available_permits(), max)
        }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DaemonConfig;

    fn test_config() -> DaemonConfig {
        let mut providers = HashMap::new();
        providers.insert(
            "test".into(),
            ai_config::ProviderConfig {
                kind: "openai".into(),
                base_url: String::new(),
                api_key: Some("k".into()),
                key_env: None,
                models: vec![],
                max_concurrency: Some(2),
            },
        );
        DaemonConfig {
            listen_addr: String::new(),
            idle_timeout_min: 0,
            providers,
            default_provider: "test".into(),
            default_model: String::new(),
            log_level: String::new(),
            tier_routing: ai_config::loader::TierRouting::default(),
        }
    }

    #[test]
    fn pool_created() {
        let mgr = ConcurrencyManager::from_config(&test_config());
        assert_eq!(mgr.limit("test"), 2);
    }

    #[tokio::test]
    async fn acquire_release() {
        let mgr = ConcurrencyManager::from_config(&test_config());
        let permit = mgr.acquire("test").await;
        assert!(permit.is_some());
        drop(permit); // releases
    }

    #[test]
    fn pool_defaults_concurrency_when_unset() {
        // A provider with max_concurrency = None should fall back to 4.
        let mut providers = HashMap::new();
        providers.insert(
            "unset".into(),
            ai_config::ProviderConfig {
                kind: "openai".into(),
                base_url: String::new(),
                api_key: Some("k".into()),
                key_env: None,
                models: vec![],
                max_concurrency: None,
            },
        );
        let cfg = DaemonConfig {
            listen_addr: String::new(),
            idle_timeout_min: 0,
            providers,
            default_provider: "unset".into(),
            default_model: String::new(),
            log_level: String::new(),
            tier_routing: ai_config::loader::TierRouting::default(),
        };
        let mgr = ConcurrencyManager::from_config(&cfg);
        assert_eq!(mgr.limit("unset"), 4);
    }
}
