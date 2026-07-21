//! Model tier abstraction — ported from auto-forge's `relay/config.rs`.
//!
//! Decouples "what capability does a profession need" (a [`ModelTier`]) from
//! "which concrete model serves that tier" (a [`ModelDefinition`] in a
//! provider's config). A profession declares its tier; the daemon resolves it
//! to a concrete model_id at request time. Swap models by editing config, not
//! code.
//!
//! Five tiers, weakest→strongest: Min < Lite < Mid < Pro < Max.

use serde::{Deserialize, Serialize};

/// Cost/performance tier for model selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    /// Ultra-cheap: high-volume, low-complexity tasks (Haiku, GPT-4o-mini).
    #[default]
    Min,
    /// Cheap: routing, chat, simple coding (Sonnet 3.5, GPT-4o).
    Lite,
    /// Balanced: planning, coding, most tasks (Sonnet 3.5, GPT-4-turbo).
    Mid,
    /// Strong: architecture, review, complex tasks (Opus, o1-preview).
    #[serde(alias = "large")]
    Pro,
    /// Ultra-strong: deepest reasoning, research (Opus 4, o1).
    #[serde(alias = "heavy")]
    Max,
}

impl ModelTier {
    pub fn display_name(self) -> &'static str {
        match self {
            ModelTier::Min => "Min",
            ModelTier::Lite => "Lite",
            ModelTier::Mid => "Mid",
            ModelTier::Pro => "Pro",
            ModelTier::Max => "Max",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            ModelTier::Min => "Ultra-cheap: high-volume, low-complexity tasks",
            ModelTier::Lite => "Cheap: routing, chat, simple coding",
            ModelTier::Mid => "Balanced: planning, coding, most tasks",
            ModelTier::Pro => "Strong: architecture, review, complex tasks",
            ModelTier::Max => "Ultra-strong: deepest reasoning, research",
        }
    }

    /// Weakest→strongest ordering: Min=0 … Max=4.
    pub fn order(self) -> u8 {
        match self {
            ModelTier::Min => 0,
            ModelTier::Lite => 1,
            ModelTier::Mid => 2,
            ModelTier::Pro => 3,
            ModelTier::Max => 4,
        }
    }

    /// Parse a tier name (case-insensitive, trims whitespace) into a
    /// [`ModelTier`]. Returns `None` for unrecognized names.
    ///
    /// This is the **single source of truth** for tier-name parsing — every
    /// call site (daemon router, agent config, .at loader) should call this
    /// instead of maintaining its own match table (see review-003 M8).
    /// Accepted aliases (beyond the snake_case enum names):
    /// `"light"` → Lite, `"large"` → Pro, `"heavy"` → Max.
    pub fn parse_name(s: &str) -> Option<ModelTier> {
        match s.trim().to_ascii_lowercase().as_str() {
            "min" => Some(ModelTier::Min),
            "lite" | "light" => Some(ModelTier::Lite),
            "mid" => Some(ModelTier::Mid),
            "pro" | "large" => Some(ModelTier::Pro),
            "max" | "heavy" => Some(ModelTier::Max),
            _ => None,
        }
    }
}

/// A concrete model entry in a provider's config, tagged with its tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDefinition {
    /// Model id sent to the API (e.g. "glm-5.2").
    pub id: String,
    /// Human-readable name (optional; defaults to the id).
    #[serde(default)]
    pub name: String,
    /// This model's capability tier.
    pub tier: ModelTier,
}

impl ModelDefinition {
    pub fn new(id: impl Into<String>, tier: ModelTier) -> Self {
        Self {
            id: id.into(),
            name: String::new(),
            tier,
        }
    }
}

/// All five tiers in weakest→strongest order.
pub fn all_tiers() -> [ModelTier; 5] {
    [
        ModelTier::Min,
        ModelTier::Lite,
        ModelTier::Mid,
        ModelTier::Pro,
        ModelTier::Max,
    ]
}

/// Resolve a desired tier to a concrete model id from a list of available
/// models. Exact-tier match first; else the **highest** available tier (so a
/// Pro request still gets served when only Max is configured, and vice-versa
/// for cost).
pub fn resolve_model_id(desired: ModelTier, available: &[ModelDefinition]) -> Option<String> {
    if available.is_empty() {
        return None;
    }
    // 1. Exact tier match.
    if let Some(m) = available.iter().find(|m| m.tier == desired) {
        return Some(m.id.clone());
    }
    // 2. Closest available: prefer the nearest tier at or above desired, else
    //    the highest below. Implementation: pick the model minimizing the
    //    tier-order gap.
    available
        .iter()
        .min_by_key(|m| {
            let gap = m.tier.order() as i16 - desired.order() as i16;
            // Prefer non-negative gaps (at-or-above) by treating them as cheaper.
            if gap >= 0 { gap } else { 1000 + gap.abs() }
        })
        .map(|m| m.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(id: &str, tier: ModelTier) -> ModelDefinition {
        ModelDefinition::new(id, tier)
    }

    #[test]
    fn tier_order() {
        assert_eq!(ModelTier::Min.order(), 0);
        assert_eq!(ModelTier::Max.order(), 4);
        assert!(ModelTier::Lite.order() < ModelTier::Pro.order());
    }

    #[test]
    fn parse_name_canonical_and_aliases() {
        // Canonical snake_case names.
        assert_eq!(ModelTier::parse_name("min"), Some(ModelTier::Min));
        assert_eq!(ModelTier::parse_name("lite"), Some(ModelTier::Lite));
        assert_eq!(ModelTier::parse_name("mid"), Some(ModelTier::Mid));
        assert_eq!(ModelTier::parse_name("pro"), Some(ModelTier::Pro));
        assert_eq!(ModelTier::parse_name("max"), Some(ModelTier::Max));
        // Aliases (must stay in sync with the serde aliases).
        assert_eq!(ModelTier::parse_name("light"), Some(ModelTier::Lite));
        assert_eq!(ModelTier::parse_name("large"), Some(ModelTier::Pro));
        assert_eq!(ModelTier::parse_name("heavy"), Some(ModelTier::Max));
        // Case-insensitive + whitespace tolerance.
        assert_eq!(ModelTier::parse_name("  MID "), Some(ModelTier::Mid));
        assert_eq!(ModelTier::parse_name("Max"), Some(ModelTier::Max));
        // Unknown → None.
        assert_eq!(ModelTier::parse_name("ultra"), None);
        assert_eq!(ModelTier::parse_name(""), None);
    }

    #[test]
    fn tier_serde_snake_case() {
        assert_eq!(serde_json::to_string(&ModelTier::Mid).unwrap(), "\"mid\"");
        assert_eq!(
            serde_json::from_str::<ModelTier>("\"max\"").unwrap(),
            ModelTier::Max
        );
    }

    #[test]
    fn tier_serde_aliases() {
        // legacy aliases from auto-forge
        assert_eq!(
            serde_json::from_str::<ModelTier>("\"large\"").unwrap(),
            ModelTier::Pro
        );
        assert_eq!(
            serde_json::from_str::<ModelTier>("\"heavy\"").unwrap(),
            ModelTier::Max
        );
    }

    #[test]
    fn resolve_exact_match() {
        let models = vec![
            md("cheap", ModelTier::Lite),
            md("mid", ModelTier::Mid),
            md("strong", ModelTier::Pro),
        ];
        assert_eq!(
            resolve_model_id(ModelTier::Mid, &models),
            Some("mid".into())
        );
    }

    #[test]
    fn resolve_falls_back_to_highest_when_no_match() {
        // Want Pro, but only Lite + Max available → Max (highest).
        let models = vec![
            md("cheap", ModelTier::Lite),
            md("max", ModelTier::Max),
        ];
        assert_eq!(
            resolve_model_id(ModelTier::Pro, &models),
            Some("max".into())
        );
    }

    #[test]
    fn resolve_prefers_nearest_above() {
        // Want Mid; Lite + Pro available → Pro (nearest above) over Lite.
        let models = vec![
            md("lite", ModelTier::Lite),
            md("pro", ModelTier::Pro),
        ];
        assert_eq!(
            resolve_model_id(ModelTier::Mid, &models),
            Some("pro".into())
        );
    }

    #[test]
    fn resolve_empty_returns_none() {
        assert_eq!(resolve_model_id(ModelTier::Max, &[]), None);
    }

    #[test]
    fn resolve_single_model_any_tier() {
        let models = vec![md("only", ModelTier::Lite)];
        assert_eq!(
            resolve_model_id(ModelTier::Max, &models),
            Some("only".into())
        );
    }

    #[test]
    fn all_tiers_ordered() {
        let tiers = all_tiers();
        for w in tiers.windows(2) {
            assert!(w[0].order() < w[1].order());
        }
    }
}
