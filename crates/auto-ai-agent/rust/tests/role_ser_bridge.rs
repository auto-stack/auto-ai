//! Plan 332 S2 — `serialize_at_role` via the auto-val serde bridge round-trips.
//!
//! The serializer now emits through `RoleDecl` (serde `Serialize`) +
//! `auto_val::node_to_at_source`, replacing the hand-written f-string emitter.
//! These tests pin the wire behaviour: full-field round-trip (the old emitter
//! silently dropped 9 of 16 fields), None-field omission, string escaping
//! (quotes/newlines corrupted the old emitter's output), tier wire form
//! (`display_name()`, what `parse_tier_field` accepts back), and the
//! roles.rs persist/validate loop (`load_role` over serialized output).

use auto_ai_agent_a2r::ai_config::ModelTier;
use auto_ai_agent_a2r::{load_role, parse_at_role, serialize_at_role, RoleConfig};

fn full_cfg() -> RoleConfig {
    RoleConfig {
        name: Some("precise-coder".into()),
        description: Some("Edits code with care".into()),
        model: Some("glm-4.7".into()),
        model_tier: Some(ModelTier::Max),
        temperature: Some(0.2),
        max_turns: Some(12),
        system_prompt: Some("You are careful.".into()),
        system_prompt_append: Some("Extra guidance.".into()),
        tools: Some(vec!["fs".into(), "shell".into()]),
        tools_append: Some(vec!["git".into()]),
        inherit: None,
        memory_limit: Some(20),
        allowed_tiers: Some(vec![ModelTier::Mid, ModelTier::Pro]),
        skills: Some(vec!["tdd".into(), "review".into()]),
        token_budget: Some(5000),
        soul_file: Some("souls/coder.md".into()),
    }
}

#[test]
fn full_field_round_trip() {
    let cfg = full_cfg();
    let src = serialize_at_role(cfg.clone());
    assert!(src.starts_with("role {"), "got: {src}");
    assert!(src.ends_with('}'), "got: {src}");
    let back = parse_at_role(&src).unwrap();
    assert_eq!(back, cfg);
}

#[test]
fn none_fields_omitted_and_empty_serializes_minimal() {
    // All-None config → bare node (skip_serializing_if drops every prop).
    let src = serialize_at_role(RoleConfig::empty());
    assert_eq!(src, "role {}");

    // Partial config: only the set field appears.
    let mut cfg = RoleConfig::empty();
    cfg.name = Some("solo".into());
    let src = serialize_at_role(cfg);
    assert!(src.contains("name : \"solo\""), "got: {src}");
    assert!(!src.contains("model"), "None field leaked: {src}");
    assert!(!src.contains("nil"), "None leaked as nil literal: {src}");
}

#[test]
fn strings_are_escaped() {
    // The old f-string emitter interpolated these raw and corrupted the file.
    let tricky = "line1\nline2 \"quoted\" \\ backslash\ttab";
    let mut cfg = RoleConfig::empty();
    cfg.system_prompt = Some(tricky.into());
    let src = serialize_at_role(cfg.clone());
    let back = parse_at_role(&src).unwrap();
    assert_eq!(back.system_prompt.as_deref(), Some(tricky));
}

#[test]
fn tier_wire_form_is_display_name() {
    let mut cfg = RoleConfig::empty();
    cfg.model_tier = Some(ModelTier::Max);
    cfg.allowed_tiers = Some(vec![ModelTier::Mid, ModelTier::Pro]);
    let src = serialize_at_role(cfg.clone());
    assert!(src.contains("model_tier : \"Max\""), "got: {src}");
    let back = parse_at_role(&src).unwrap();
    assert_eq!(back.model_tier, Some(ModelTier::Max));
    assert_eq!(back.allowed_tiers, Some(vec![ModelTier::Mid, ModelTier::Pro]));
}

#[test]
fn previously_dropped_fields_now_round_trip() {
    // The old emitter only wrote 7 of 16 fields; these silently vanished on
    // save. They now survive the trip.
    let mut cfg = RoleConfig::empty();
    cfg.description = Some("d".into());
    cfg.tools = Some(vec!["fs".into()]);
    cfg.tools_append = Some(vec!["git".into()]);
    cfg.skills = Some(vec!["s".into()]);
    cfg.memory_limit = Some(20);
    cfg.token_budget = Some(100);
    cfg.soul_file = Some("soul.md".into());
    cfg.system_prompt_append = Some("append".into());
    let back = parse_at_role(&serialize_at_role(cfg.clone())).unwrap();
    assert_eq!(back, cfg);
}

#[test]
fn load_role_over_serialized_output() {
    // The roles.rs persist/validate loop: serialize → load back must succeed
    // (roles.rs re-parses serialized configs before constructing ConfigRole).
    let src = serialize_at_role(full_cfg());
    assert!(load_role(&src).is_ok());
}
