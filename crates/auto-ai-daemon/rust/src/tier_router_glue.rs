//! Hand-written glue for tier_router.at.
//!
//! `config.tier_routing.routes` can't be expressed in `.at` source — `routes`
//! is a reserved keyword in Auto (routing/navigation), so the field access
//! `tr.routes` is a parse error there. This module exposes the routing entries
//! as an owned `Vec<(String, Vec<TierRouteCandidate>)>` that `tier_router.at`
//! iterates via a `use.rust`-bridged call. (Plan 025 Phase 1: small glue for an
//! a2r blocker, mirroring auto-ai-agent's client_impl.rs pattern.)

use ai_config::loader::{TierRouteCandidate, TierRouting};

/// The `(tier_name, candidates)` pairs in a TierRouting table, owned so the .at
/// caller can iterate without borrowing across the `.routes` keyword boundary.
pub fn tier_route_entries(tr: &TierRouting) -> Vec<(String, Vec<TierRouteCandidate>)> {
    tr.routes
        .iter()
        .map(|(tier, cands)| (tier.clone(), cands.clone()))
        .collect()
}
