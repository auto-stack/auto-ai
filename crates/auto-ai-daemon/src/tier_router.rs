//! Tier Router — multi-provider model routing with fallback.
//!
//! (Multi-Provider Model Routing design)
//!
//! Replaces the old single-default-provider tier resolution with a
//! configurable tier→candidate-chain mapping. Each tier can have multiple
//! candidates across providers (primary → fallback 1 → fallback 2…).
//! The router tries them in order, falling back on 429/timeout.
//!
//! Backward compatible: if no `tier_routing` is configured, falls back to
//! `default_provider` + `resolve_model_id` (the old behavior).

use std::collections::HashMap;

use ai_config::{DaemonConfig, ModelDefinition, ModelTier, ProviderConfig};

/// A single candidate in a tier's routing chain.
#[derive(Debug, Clone)]
pub struct TierCandidate {
    pub provider: String,
    pub model: String,
}

/// The tier router — resolves a tier to an ordered list of candidates.
#[derive(Debug, Clone, Default)]
pub struct TierRouter {
    /// tier → ordered candidates (primary first).
    routing: HashMap<ModelTier, Vec<TierCandidate>>,
}

impl TierRouter {
    /// Build a router from the daemon config. If `tier_routing` is explicitly
    /// configured in the .at file, use it. Otherwise auto-derive from providers
    /// (default_provider's models first, others as fallbacks).
    pub fn from_config(config: &DaemonConfig) -> Self {
        // 1. If explicit tier_routing is configured, use it directly.
        if !config.tier_routing.is_empty() {
            let mut routing: HashMap<ModelTier, Vec<TierCandidate>> = HashMap::new();
            for (tier_name, candidates) in &config.tier_routing.routes {
                let tier = match tier_name.as_str() {
                    "min" => ModelTier::Min,
                    "lite" | "light" => ModelTier::Lite,
                    "mid" => ModelTier::Mid,
                    "pro" | "large" => ModelTier::Pro,
                    "max" | "heavy" => ModelTier::Max,
                    _ => continue,
                };
                let tc: Vec<TierCandidate> = candidates.iter()
                    .map(|c| TierCandidate {
                        provider: c.provider.clone(),
                        model: c.model.clone(),
                    })
                    .collect();
                routing.insert(tier, tc);
            }
            tracing::info!("tier_router: using explicit tier_routing from config ({} tiers)", routing.len());
            return Self { routing };
        }

        // 2. Auto-derive from providers (legacy behavior).
        let mut routing: HashMap<ModelTier, Vec<TierCandidate>> = HashMap::new();

        // Default provider's models go first per tier.
        if let Some(default) = config.providers.get(&config.default_provider) {
            for m in &default.models {
                routing
                    .entry(m.tier)
                    .or_default()
                    .push(TierCandidate {
                        provider: config.default_provider.clone(),
                        model: m.id.clone(),
                    });
            }
        }

        // Other providers' models go as fallbacks.
        for (name, pc) in &config.providers {
            if name == &config.default_provider {
                continue;
            }
            for m in &pc.models {
                // Only add if this tier+provider combo isn't already there.
                let exists = routing
                    .get(&m.tier)
                    .map(|cands| cands.iter().any(|c| c.provider == *name))
                    .unwrap_or(false);
                if !exists {
                    routing
                        .entry(m.tier)
                        .or_default()
                        .push(TierCandidate {
                            provider: name.clone(),
                            model: m.id.clone(),
                        });
                }
            }
        }

        Self { routing }
    }

    /// Resolve a tier to its candidate chain. Returns the ordered list
    /// (primary first, fallbacks after). Empty = no candidates configured.
    pub fn candidates(&self, tier: ModelTier) -> &[TierCandidate] {
        self.routing.get(&tier).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Resolve a `"tier:max"` token to (provider, model), considering
    /// optional provider preference. Returns the first candidate.
    /// The caller should handle fallback by iterating `candidates()`.
    pub fn resolve(
        &self,
        tier: ModelTier,
        preferred_provider: Option<&str>,
    ) -> Option<&TierCandidate> {
        let cands = self.candidates(tier);
        if cands.is_empty() {
            return None;
        }
        // If a preferred provider is specified, try it first.
        if let Some(pref) = preferred_provider {
            if let Some(c) = cands.iter().find(|c| c.provider == pref) {
                return Some(c);
            }
        }
        // Default: first candidate (primary).
        cands.first()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_config::{ModelDefinition, ProviderConfig};

    fn mock_config() -> DaemonConfig {
        let mut providers = HashMap::new();
        providers.insert("zhipu".into(), ProviderConfig {
            kind: "anthropic".into(),
            base_url: "http://zhipu".into(),
            api_key: None,
            key_env: None,
            models: vec![
                ModelDefinition::new("glm-5.2", ModelTier::Max),
                ModelDefinition::new("glm-5-turbo", ModelTier::Mid),
            ],
            max_concurrency: Some(4),
        });
        providers.insert("deepseek".into(), ProviderConfig {
            kind: "anthropic".into(),
            base_url: "http://deepseek".into(),
            api_key: None,
            key_env: None,
            models: vec![
                ModelDefinition::new("deepseek-v4-pro", ModelTier::Pro),
                ModelDefinition::new("deepseek-v4-flash", ModelTier::Lite),
            ],
            max_concurrency: Some(4),
        });
        DaemonConfig {
            listen_addr: "127.0.0.1:17654".into(),
            idle_timeout_min: 10,
            log_level: "info".into(),
            providers,
            default_provider: "zhipu".into(),
            default_model: "glm-5.2".into(),
        }
    }

    #[test]
    fn auto_derive_routing_from_providers() {
        let cfg = mock_config();
        let router = TierRouter::from_config(&cfg);

        // Max tier: zhipu (default) first.
        let max = router.candidates(ModelTier::Max);
        assert_eq!(max.len(), 1);
        assert_eq!(max[0].provider, "zhipu");
        assert_eq!(max[0].model, "glm-5.2");

        // Pro tier: deepseek only (zhipu has no Pro model).
        let pro = router.candidates(ModelTier::Pro);
        assert_eq!(pro.len(), 1);
        assert_eq!(pro[0].provider, "deepseek");
        assert_eq!(pro[0].model, "deepseek-v4-pro");

        // Mid tier: zhipu (default) first.
        let mid = router.candidates(ModelTier::Mid);
        assert_eq!(mid.len(), 1);
        assert_eq!(mid[0].provider, "zhipu");
    }

    #[test]
    fn resolve_with_preferred_provider() {
        let cfg = mock_config();
        let router = TierRouter::from_config(&cfg);

        // No preference → first candidate (zhipu).
        let r = router.resolve(ModelTier::Max, None).unwrap();
        assert_eq!(r.provider, "zhipu");

        // Prefer deepseek for Max → not available, falls back to zhipu.
        let r = router.resolve(ModelTier::Max, Some("deepseek"));
        assert!(r.is_none() || r.unwrap().provider == "zhipu");
    }

    #[test]
    fn empty_router_returns_none() {
        let router = TierRouter::default();
        assert!(router.candidates(ModelTier::Max).is_empty());
        assert!(router.resolve(ModelTier::Max, None).is_none());
    }
}
