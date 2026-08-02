//! Shared AI configuration + canonical wire types for AutoOS.
//!
//! Three consumers:
//! - `auto-ai-client` — sends canonical [`wire::CompletionRequest`]s to the
//!   daemon.
//! - `auto-ai-daemon` — receives canonical requests and translates them to a
//!   concrete provider's format.
//! - `auto-ai-agent` — validates Profession models against the loaded config.
//!
//! Defining the canonical types once here keeps the client↔daemon boundary
//! provider-neutral (no OpenAI/Anthropic shapes leak across it).

pub mod loader;
pub mod provider;
pub mod tier;
pub mod validate;
pub mod wire;

pub use loader::{parse_client_config, parse_daemon_config, ClientConfig, ConfigError, DaemonConfig};
pub use provider::ProviderConfig;
pub use tier::{all_tiers, resolve_model_id, ModelDefinition, ModelTier};
pub use validate::validate_model_exists;
pub use wire::*;
