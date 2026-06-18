//! Canonical wire types — re-exported from `ai-config`.
//!
//! The definitions live in the `ai-config` crate (single source of truth for
//! the client↔daemon wire format). This thin module exists so existing
//! `use crate::types::*` paths inside the client (and its provider code, which
//! moves to the daemon in a later task) keep resolving. New code should import
//! from `ai_config` directly, or via the crate root (`crate::*`).

pub use ai_config::{
    CompletionRequest, CompletionResponse, ContentBlock, Message, ToolCall, ToolDefinition, Usage,
};
