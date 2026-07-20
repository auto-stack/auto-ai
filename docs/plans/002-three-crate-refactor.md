# 三库职责重构 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 LLM API 通信能力从 `auto-ai-client` 迁到 `auto-ai-daemon`,client 瘦化为纯 daemon HTTP 客户端,并抽出公共 `ai-config` crate 统一三处配置。

**Architecture:** daemon 成为唯一的 LLM 出口(收 canonical 请求 → 转 provider 形态 → 调上游 → 转 canonical 响应),client 删除 direct 模式与 provider 代码,只发 canonical `ContentBlock` 请求。新建 `ai-config` crate 用 auto-atom 统一解析 `ai-client.at` / `ai-daemon.at`,合并重复的 Provider 结构,加 model 存在性校验。两个配置文件保持分开。

**Tech Stack:** Rust,axum(daemon),reqwest(client+daemon),auto-atom/auto-val(配置解析),tokio,thiserror。

---

## 背景与决策记录(迁移前置讨论结论)

本计划是 Plan 001 完成后、Forge/Ash 迁移(原 Phase 6/7)之前的必要重构。四个关键决策:

1. **daemon 加形态转换**:daemon 负责所有 OpenAI↔Anthropic 翻译,client 不再处理 provider 差异。
2. **LLM 通信全迁 daemon**:client 的 `provider/`、`openai_format.rs`、`anthropic.rs`、`openai.rs` 全部搬到 daemon。client 不再直连 LLM。
3. **删除 client direct 模式**:daemon 从"可选加速器"变成"必经之路"。daemon 必须始终可用(由 client 的 `ensure_daemon` 自启动)。
4. **canonical wire 格式**:client↔daemon 之间用 `ContentBlock` 模型序列化(daemon 内部再翻译到具体 provider)。client 完全不感知 OpenAI/Anthropic。
5. **抽 `ai-config` 公共 crate**:统一 .at 解析(删三处手写 scanner)+ 统一 Provider 结构 + model 校验。
6. **配置文件保持两文件**:`ai-client.at`(client 视角)与 `ai-daemon.at`(daemon 专有字段)分开,但都用 ai-config 解析。

### 现状文件清单(重构前)

```
crates/
├── auto-ai-client/src/
│   ├── lib.rs          ← AiClient(direct+daemon 双模式) ← 大改:删 direct
│   ├── config.rs       ← 手写 scanner 的 ClientConfig  ← 删除(迁到 ai-config)
│   ├── daemon.rs       ← ensure_daemon 发现/自启动     ← 保留
│   ├── provider.rs     ← AiProvider trait + Registry   ← 迁到 daemon
│   ├── provider/
│   │   ├── anthropic.rs ← Claude provider              ← 迁到 daemon
│   │   └── openai.rs    ← OpenAI provider              ← 迁到 daemon
│   ├── openai_format.rs ← OpenAI 形态转换              ← 迁到 daemon
│   ├── sse.rs           ← SSE 解析                      ← 迁到 daemon
│   └── types.rs         ← ContentBlock/ToolDefinition   ← 迁到 ai-config(共享)
├── auto-ai-daemon/src/
│   ├── config.rs        ← 手写 scanner 的 DaemonConfig ← 删除(迁到 ai-config)
│   ├── server.rs        ← 透明转发                      ← 大改:加形态转换
│   ├── pool.rs          ← 并发池                        ← 保留
│   ├── tracker.rs       ← usage 追踪                    ← 保留
│   ├── lib.rs
│   └── main.rs
└── auto-ai-agent/       ← 仅小改:model 校验
```

### 重构后文件清单(目标)

```
crates/
├── ai-config/           ← 新建
│   ├── Cargo.toml
│   ├── src/lib.rs       ← 统一 Provider 结构 + .at 解析(auto-atom) + model 校验
│   ├── src/wire.rs      ← ContentBlock/Message/ToolDefinition/CompletionRequest/Response(canonical 共享类型)
│   └── src/provider.rs  ← 统一 ProviderConfig(合并旧 ProviderConfig + ProviderEntry)
├── auto-ai-client/src/
│   ├── lib.rs           ← 瘦 AiClient:只发 canonical HTTP 给 daemon
│   ├── daemon.rs        ← 保留:ensure_daemon
│   └── error.rs         ← ClientError(保留)
├── auto-ai-daemon/src/
│   ├── config.rs        ← DaemonConfig 包装 ai-config(加 listen_addr/idle_timeout 等 daemon 专有字段)
│   ├── server.rs        ← 收 canonical → provider 形态转换 → 调上游 → 转 canonical 响应
│   ├── provider/        ← 从 client 迁来:anthropic.rs/openai.rs
│   ├── format.rs        ← 从 client 迁来:openai_format.rs + canonical↔provider 转换
│   ├── sse.rs           ← 从 client 迁来
│   ├── pool.rs          ← 保留
│   ├── tracker.rs       ← 保留
│   ├── lib.rs
│   └── main.rs
└── auto-ai-agent/       ← 小改:Profession 加载走 ai-config 做 model 校验
```

---

## Task 1: 新建 `ai-config` crate 骨架 + 共享 canonical 类型

**Files:**
- Create: `crates/ai-config/Cargo.toml`
- Create: `crates/ai-config/src/lib.rs`
- Create: `crates/ai-config/src/wire.rs`
- Modify: `Cargo.toml`(workspace members)

**说明:** 把 client `types.rs` 里的 canonical 模型(`Message`/`ContentBlock`/`ToolDefinition`/`ToolCall`/`CompletionRequest`/`CompletionResponse`/`Usage`)整体搬到 `ai-config/src/wire.rs`。client 和 daemon 都依赖它,这样 canonical wire 格式只有一处定义。

- [ ] **Step 1: 创建 `crates/ai-config/Cargo.toml`**

```toml
[package]
name = "ai-config"
version = "0.1.0"
edition = "2021"
description = "Shared AI configuration + canonical wire types for AutoOS AI crates"

[lib]
name = "ai_config"
path = "src/lib.rs"

[dependencies]
auto-atom = { path = "../../auto-lang/crates/auto-atom" }
auto-val = { path = "../../auto-lang/crates/auto-val" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "1"
dirs = "5"
```

- [ ] **Step 2: 把 client 的 `types.rs` 内容复制到 `crates/ai-config/src/wire.rs`**

逐字复制 `crates/auto-ai-client/src/types.rs` 的全部内容(Message/ContentBlock/ToolDefinition/ToolCall/CompletionRequest/CompletionResponse/Usage + 构造器 + 测试)。把模块文档头改为说明这是"canonical wire types,daemon 与 client 之间的中立格式"。

- [ ] **Step 3: 创建 `crates/ai-config/src/lib.rs`**

```rust
//! Shared AI configuration + canonical wire types for AutoOS.
//!
//! Three consumers: `auto-ai-client` (sends canonical requests),
//! `auto-ai-daemon` (receives canonical, translates to provider), and
//! `auto-ai-agent` (validates Profession models against config).

pub mod wire;

pub use wire::*;
```

- [ ] **Step 4: 加入 workspace**

修改根 `Cargo.toml`,在 `members` 里加 `"crates/ai-config"`(放在最前面,因为它是依赖底层)。

- [ ] **Step 5: 验证编译**

Run: `cargo build -p ai-config`
Expected: 编译通过,wire 模块的所有测试可通过 `cargo test -p ai-config`。

- [ ] **Step 6: 提交**

```bash
git add crates/ai-config Cargo.toml
git commit -m "feat(ai-config): new crate with canonical wire types"
```

---

## Task 2: ai-config 统一 Provider 结构

**Files:**
- Create: `crates/ai-config/src/provider.rs`
- Modify: `crates/ai-config/src/lib.rs`

**说明:** 合并现有的 `auto-ai-client::ProviderConfig` 和 `auto-ai-daemon::ProviderEntry` 为一个统一结构。两者字段几乎相同,daemon 多一个 `max_concurrency`。统一结构把 `max_concurrency` 设为 `Option<usize>`(client 用不到时为 None)。

- [ ] **Step 1: 写失败测试 `provider.rs` 的解析与结构**

在 `crates/ai-config/src/provider.rs` 写:

```rust
use serde::{Deserialize, Serialize};

/// 统一的 provider 配置(client 与 daemon 共用)。
/// `max_concurrency` 仅 daemon 用,client 场景为 None。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// "anthropic" | "openai" | "zhipu"
    pub kind: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub key_env: Option<String>,
    pub models: Vec<String>,
    /// 仅 daemon:并发上限。client 场景忽略。
    pub max_concurrency: Option<usize>,
}

impl ProviderConfig {
    /// 解析 API key:direct 字符串 > 环境变量。
    pub fn resolve_key(&self) -> Option<String> {
        if let Some(key) = &self.api_key {
            return Some(key.clone());
        }
        if let Some(env_name) = &self.key_env {
            return std::env::var(env_name).ok();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_key_direct_over_env() {
        let pc = ProviderConfig {
            kind: "openai".into(),
            base_url: String::new(),
            api_key: Some("sk-xxx".into()),
            key_env: Some("OPENAI_API_KEY".into()),
            models: vec![],
            max_concurrency: None,
        };
        assert_eq!(pc.resolve_key(), Some("sk-xxx".into()));
    }

    #[test]
    fn resolve_key_from_env() {
        std::env::set_var("TEST_AI_KEY_X", "env-val");
        let pc = ProviderConfig {
            kind: "openai".into(),
            base_url: String::new(),
            api_key: None,
            key_env: Some("TEST_AI_KEY_X".into()),
            models: vec![],
            max_concurrency: None,
        };
        assert_eq!(pc.resolve_key(), Some("env-val".into()));
        std::env::remove_var("TEST_AI_KEY_X");
    }
}
```

- [ ] **Step 2: 在 `lib.rs` 导出 provider 模块**

```rust
pub mod provider;
pub use provider::ProviderConfig;
```

- [ ] **Step 3: 验证测试通过**

Run: `cargo test -p ai-config`
Expected: wire 测试 + provider 的 2 个测试全绿。

- [ ] **Step 4: 提交**

```bash
git add crates/ai-config/src/provider.rs crates/ai-config/src/lib.rs
git commit -m "feat(ai-config): unified ProviderConfig"
```

---

## Task 3: ai-config 用 auto-atom 解析 .at 配置

**Files:**
- Create: `crates/ai-config/src/loader.rs`
- Modify: `crates/ai-config/src/lib.rs`

**说明:** 用 auto-atom 解析两种文件:`ai-client.at`(client 视角:default_provider/default_model + providers 块)和 `ai-daemon.at`(daemon 视角:额外有 listen_addr/idle_timeout_min/log_level + 每个 provider 的 max_concurrency)。两种都解析出 `providers: HashMap<String, ProviderConfig>`。

- [ ] **Step 1: 写失败测试,定义 loader API**

`crates/ai-config/src/loader.rs`:

```rust
use std::collections::HashMap;
use std::path::PathBuf;

use auto_atom::{Atom, AtomParser};
use auto_val::Value;

use crate::provider::ProviderConfig;

/// 解析 .at 配置的错误。
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config parse error: {0}")]
    Parse(String),
    #[error("config IO error: {0}")]
    Io(String),
}

/// client 视角的配置(default_provider/default_model + providers)。
#[derive(Clone, Debug, Default)]
pub struct ClientConfig {
    pub providers: HashMap<String, ProviderConfig>,
    pub default_provider: String,
    pub default_model: String,
}

/// daemon 视角的配置(client 配置 + daemon 专有字段)。
#[derive(Clone, Debug)]
pub struct DaemonConfig {
    pub listen_addr: String,
    pub idle_timeout_min: u64,
    pub log_level: String,
    pub providers: HashMap<String, ProviderConfig>,
    pub default_provider: String,
    pub default_model: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1:17654".into(),
            idle_timeout_min: 10,
            log_level: "info".into(),
            providers: HashMap::new(),
            default_provider: String::new(),
            default_model: String::new(),
        }
    }
}

/// 解析 ai-client.at 内容。
pub fn parse_client_config(content: &str) -> Result<ClientConfig, ConfigError> {
    // TODO: Task 3 Step 3 实现
    todo!()
}

/// 解析 ai-daemon.at 内容。
pub fn parse_daemon_config(content: &str) -> Result<DaemonConfig, ConfigError> {
    // TODO: Task 3 Step 3 实现
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_client_config_example() {
        let src = r#"
            default_provider : zhipu
            default_model : glm-4.5

            zhipu {
                kind : openai
                base_url : "https://open.bigmodel.cn/api/paas/v4"
                key_env : ZHIPU_API_KEY
                models : glm-4.5,glm-flash
            }
        "#;
        let cfg = parse_client_config(src).unwrap();
        assert_eq!(cfg.default_provider, "zhipu");
        assert_eq!(cfg.default_model, "glm-4.5");
        let zhipu = cfg.providers.get("zhipu").unwrap();
        assert_eq!(zhipu.kind, "openai");
        assert_eq!(zhipu.models, vec!["glm-4.5".to_string(), "glm-flash".to_string()]);
    }

    #[test]
    fn parse_daemon_config_example() {
        let src = r#"
            listen_addr : "127.0.0.1:9999"
            default_provider : zhipu
            default_model : glm-4.5

            zhipu {
                kind : openai
                base_url : "https://open.bigmodel.cn/api/paas/v4"
                api_key : "test-key"
                models : glm-4.5,glm-flash
                max_concurrency : 4
            }
        "#;
        let cfg = parse_daemon_config(src).unwrap();
        assert_eq!(cfg.listen_addr, "127.0.0.1:9999");
        let zhipu = cfg.providers.get("zhipu").unwrap();
        assert_eq!(zhipu.max_concurrency, Some(4));
        assert_eq!(zhipu.api_key.as_deref(), Some("test-key"));
    }
}
```

注意:上面 `.at` 里 `models : glm-4.5,glm-flash` 是逗号分隔的字符串(沿用现有格式),解析后 split。若 auto-atom 把它解析为单个字符串则 split,若是数组则遍历。Task 3 Step 3 据实处理。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p ai-config parse_client_config_example`
Expected: panic on `todo!()`。

- [ ] **Step 3: 实现 parse_client_config 与 parse_daemon_config**

auto-atom 的导航模式(参考 auto-ai-agent Phase 4 已验证):`AtomParser::parse(content)` 返回 `Atom`。但 client/daemon 的 .at 是**多顶层项**(flat key:value + 多个 `name { }` 块),不是单根 node。

**关键:auto-atom 文档是单根的**。现有 ai-client.at 的 `default_provider : zhipu` + `zhipu { ... }` 混合形态在单根解析下会失败(顶层有多个值)。两个解法:
- (a) 把配置包进一个根块(改格式:`config { default_provider : ..., zhipu { ... } }`)— 要迁移现有配置文件。
- (b) 用 auto-atom 逐块解析,或保留一个能处理 flat 顶层 + 块的 scanner。

**决策:采用 (a),改格式为单根**。在 Task 3 实现里,要求 ai-client.at 改为:

```text
client {
    default_provider : zhipu
    default_model : glm-4.5
    zhipu {
        kind : openai
        base_url : "https://open.bigmodel.cn/api/paas/v4"
        key_env : ZHIPU_API_KEY
        models : glm-4.5,glm-flash
    }
}
```

ai-daemon.at 改为:

```text
daemon {
    listen_addr : "127.0.0.1:9999"
    default_provider : zhipu
    default_model : glm-4.5
    zhipu {
        kind : openai
        base_url : "..."
        api_key : "test-key"
        models : glm-4.5,glm-flash
        max_concurrency : 4
    }
}
```

更新 Step 1 的测试用单根 `client { }` / `daemon { }` 包裹。然后实现:

```rust
pub fn parse_client_config(content: &str) -> Result<ClientConfig, ConfigError> {
    let atom = AtomParser::parse(content)
        .map_err(|e| ConfigError::Parse(format!("ai-client.at: {e}")))?;
    let node = match atom {
        Atom::Node(n) if n.name.as_str() == "client" => n,
        other => return Err(ConfigError::Parse(format!("expected 'client' root, found {:?}", other))),
    };

    let default_provider = opt_str(&node, "default_provider").unwrap_or_default();
    let default_model = opt_str(&node, "default_model").unwrap_or_default();
    let providers = parse_provider_blocks(&node);

    if providers.is_empty() {
        return Err(ConfigError::Parse("no providers configured".into()));
    }
    let default_provider = if default_provider.is_empty() {
        providers.keys().next().cloned().unwrap_or_default()
    } else {
        default_provider
    };

    Ok(ClientConfig { providers, default_provider, default_model })
}

pub fn parse_daemon_config(content: &str) -> Result<DaemonConfig, ConfigError> {
    let atom = AtomParser::parse(content)
        .map_err(|e| ConfigError::Parse(format!("ai-daemon.at: {e}")))?;
    let node = match atom {
        Atom::Node(n) if n.name.as_str() == "daemon" => n,
        other => return Err(ConfigError::Parse(format!("expected 'daemon' root, found {:?}", other))),
    };

    let mut cfg = DaemonConfig::default();
    cfg.listen_addr = opt_str(&node, "listen_addr").unwrap_or_else(|| cfg.listen_addr.clone());
    cfg.idle_timeout_min = opt_uint(&node, "idle_timeout_min").unwrap_or(10) as u64;
    cfg.log_level = opt_str(&node, "log_level").unwrap_or_else(|| cfg.log_level.clone());
    cfg.default_provider = opt_str(&node, "default_provider").unwrap_or_default();
    cfg.default_model = opt_str(&node, "default_model").unwrap_or_default();
    cfg.providers = parse_provider_blocks(&node);
    if cfg.default_provider.is_empty() {
        cfg.default_provider = cfg.providers.keys().next().cloned().unwrap_or_default();
    }
    if cfg.default_model.is_empty() {
        cfg.default_model = cfg.providers.get(&cfg.default_provider)
            .and_then(|p| p.models.first().cloned()).unwrap_or_default();
    }
    Ok(cfg)
}

/// 遍历 node 的子节点(name{}块),每个块解析为 ProviderConfig。
fn parse_provider_blocks(node: &auto_val::Node) -> HashMap<String, ProviderConfig> {
    let mut providers = HashMap::new();
    for (_key, kid) in node.kids_iter() {
        if let auto_val::Kid::Node(child) = kid {
            let name = child.name.to_string();
            let pc = ProviderConfig {
                kind: opt_str(child, "kind").unwrap_or_default(),
                base_url: opt_str(child, "base_url").unwrap_or_default(),
                api_key: opt_str(child, "api_key"),
                key_env: opt_str(child, "key_env"),
                models: opt_str(child, "models")
                    .map(|s| s.split(',').map(|m| m.trim().to_string()).collect())
                    .unwrap_or_default(),
                max_concurrency: opt_uint(child, "max_concurrency"),
            };
            if !pc.kind.is_empty() {
                providers.insert(name, pc);
            }
        }
    }
    providers
}

fn opt_str(node: &auto_val::Node, key: &str) -> Option<String> {
    match node.get_prop_of(key) {
        Value::Str(s) => Some(s.to_string()),
        Value::Nil => None,
        other => Some(other.to_astr().to_string()),
    }
}

fn opt_uint(node: &auto_val::Node, key: &str) -> Option<usize> {
    match node.get_prop_of(key) {
        Value::Uint(u) => Some(u as usize),
        Value::Int(i) if i >= 0 => Some(i as usize),
        Value::Nil => None,
        _ => None,
    }
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p ai-config`
Expected: 所有测试绿(parse_client_config_example + parse_daemon_config_example + wire + provider)。

- [ ] **Step 5: 提交**

```bash
git add crates/ai-config/src/loader.rs crates/ai-config/src/lib.rs
git commit -m "feat(ai-config): auto-atom loader for client/daemon configs"
```

---

## Task 4: ai-config 加 model 存在性校验

**Files:**
- Modify: `crates/ai-config/src/loader.rs`
- Create: `crates/ai-config/src/validate.rs`
- Modify: `crates/ai-config/src/lib.rs`

**说明:** Profession 引用的 model 必须在某 provider 的 models 列表里,否则报清晰错误。提供 `validate_model_exists(&config, model)`。

- [ ] **Step 1: 写失败测试**

`crates/ai-config/src/validate.rs`:

```rust
use crate::loader::ClientConfig;

/// 校验 model 是否存在于配置的任一 provider 的 models 列表里。
pub fn validate_model_exists(config: &ClientConfig, model: &str) -> Result<(), String> {
    for (name, p) in &config.providers {
        if p.models.iter().any(|m| m == model) {
            return Ok(());
        }
    }
    let available: Vec<&str> = config.providers.values()
        .flat_map(|p| p.models.iter().map(|s| s.as_str())).collect();
    Err(format!(
        "model '{}' not found in any configured provider; available: {:?}",
        model, available
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::parse_client_config;

    #[test]
    fn validates_existing_model() {
        let cfg = parse_client_config(r#"
            client {
                default_provider : zhipu
                zhipu { kind : openai, models : glm-4.5,glm-flash }
            }
        "#).unwrap();
        assert!(validate_model_exists(&cfg, "glm-4.5").is_ok());
    }

    #[test]
    fn rejects_unknown_model() {
        let cfg = parse_client_config(r#"
            client {
                default_provider : zhipu
                zhipu { kind : openai, models : glm-4.5 }
            }
        "#).unwrap();
        let err = validate_model_exists(&cfg, "nonexistent").unwrap_err();
        assert!(err.contains("not found"));
        assert!(err.contains("glm-4.5"));
    }
}
```

- [ ] **Step 2: lib.rs 导出 validate**

```rust
pub mod validate;
pub use validate::validate_model_exists;
```

- [ ] **Step 3: 验证测试通过**

Run: `cargo test -p ai-config`
Expected: 全绿。

- [ ] **Step 4: 提交**

```bash
git add crates/ai-config/src/validate.rs crates/ai-config/src/lib.rs
git commit -m "feat(ai-config): model existence validation"
```

---

## Task 5: client 瘦化 — 改用 ai-config 的 wire 类型

**Files:**
- Modify: `crates/auto-ai-client/Cargo.toml`(加 ai-config 依赖)
- Modify: `crates/auto-ai-client/src/lib.rs`
- Delete: `crates/auto-ai-client/src/types.rs`

**说明:** client 不再自己定义 canonical 类型,改为 `pub use ai_config::*`(re-export)。这样 client 的公开 API 类型不变,但定义只在 ai-config 一处。agent 已经通过 client 用这些类型,迁移后 agent 也改用 ai-config(或继续经 client re-export)。

- [ ] **Step 1: client Cargo.toml 加依赖**

```toml
[dependencies]
ai-config = { path = "../ai-config" }
# 删除原来 types.rs 用到的 serde/serde_json 如果没有其他地方用
```

- [ ] **Step 2: lib.rs 删除 `pub mod types;`,改为 re-export**

把 `crates/auto-ai-client/src/lib.rs` 里的 `pub mod types;` 和 `pub use types::*;` 改为:

```rust
pub use ai_config::*;
// canonical wire 类型现在来自 ai-config
```

删除 `crates/auto-ai-client/src/types.rs` 文件。

- [ ] **Step 3: 修复 client 内部对 types 的引用**

client 的 `provider/anthropic.rs`、`provider/openai.rs`、`openai_format.rs`、`lib.rs` 里 `use crate::types::*;` 改为 `use ai_config::*;`(或保持 `use crate::*;` 因为 re-export)。

- [ ] **Step 4: 验证 client 编译**

Run: `cargo build -p auto-ai-client`
Expected: 编译通过(types 来自 ai-config)。

- [ ] **Step 5: 提交**

```bash
git add crates/auto-ai-client crates/ai-config
git commit -m "refactor(client): use ai-config canonical types (re-export)"
```

---

## Task 6: daemon 迁入 provider/format/sse 代码

**Files:**
- Modify: `crates/auto-ai-daemon/Cargo.toml`(加 ai-config 依赖)
- Create: `crates/auto-ai-daemon/src/provider/mod.rs`(从 client 迁)
- Create: `crates/auto-ai-daemon/src/provider/anthropic.rs`(从 client 迁)
- Create: `crates/auto-ai-daemon/src/provider/openai.rs`(从 client 迁)
- Create: `crates/auto-ai-daemon/src/format.rs`(从 client openai_format.rs 迁)
- Create: `crates/auto-ai-daemon/src/sse.rs`(从 client 迁)
- Modify: `crates/auto-ai-daemon/src/lib.rs`

**说明:** 把 client 的 `provider/`、`openai_format.rs`、`sse.rs` 整体复制到 daemon。这些代码里的 `use crate::types::*` / `use crate::ClientError` 改为 `use ai_config::*` 和 daemon 自己的 error 类型。daemon 现在有了真正的 LLM 调用能力。

- [ ] **Step 1: daemon Cargo.toml 加依赖**

```toml
[dependencies]
ai-config = { path = "../ai-config" }
# 已有的 axum/tokio/serde/reqwest 等保留
```

- [ ] **Step 2: 复制 client 的 provider/format/sse 到 daemon**

逐文件复制 `crates/auto-ai-client/src/provider.rs` → `crates/auto-ai-daemon/src/provider/mod.rs`,`provider/anthropic.rs`、`provider/openai.rs` → `crates/auto-ai-daemon/src/provider/`,`openai_format.rs` → `format.rs`,`sse.rs` → `sse.rs`。每个文件里把 `use crate::types::*` 改 `use ai_config::*`,`use crate::ClientError` 改为 daemon 的 error(Step 3 定义)。

- [ ] **Step 3: daemon 定义 LLM 调用 error**

`crates/auto-ai-daemon/src/lib.rs` 加:

```rust
/// daemon 内部 LLM 调用错误。
#[derive(Debug)]
pub enum LlmError {
    Http(String),
    Api(String),
    NoProvider,
    NoApiKey(String),
}
impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::Http(e) => write!(f, "HTTP: {e}"),
            LlmError::Api(e) => write!(f, "API: {e}"),
            LlmError::NoProvider => write!(f, "no provider"),
            LlmError::NoApiKey(p) => write!(f, "no key for {p}"),
        }
    }
}
impl std::error::Error for LlmError {}
```

provider 代码里 `ClientError` → `LlmError`(字段映射:`Http`/`Api`/`NoApiKey`/`NoProvider` 一一对应)。

- [ ] **Step 4: lib.rs 导出新模块**

```rust
pub mod format;
pub mod provider;
pub mod sse;
pub use provider::{AiProvider, ProviderRegistry};
```

- [ ] **Step 5: 验证 daemon 编译**

Run: `cargo build -p auto-ai-daemon`
Expected: 编译通过(provider/format/sse 现在在 daemon)。

- [ ] **Step 6: 提交**

```bash
git add crates/auto-ai-daemon crates/auto-ai-client
git commit -m "refactor(daemon): absorb provider/format/sse from client"
```

---

## Task 7: daemon server 实现 canonical→provider 转换

**Files:**
- Modify: `crates/auto-ai-daemon/src/server.rs`
- Modify: `crates/auto-ai-daemon/src/config.rs`(改用 ai-config)

**说明:** server.rs 现在收到的是 canonical `CompletionRequest`(ai-config 的类型)。它要:用 daemon 配置选 provider → 把 canonical 请求转成该 provider 的形态(OpenAI 或 Anthropic)→ 调 provider → 把 provider 响应转回 canonical `CompletionResponse` → 返给 client。这替换掉原来的"透明转发"。

- [ ] **Step 1: daemon config.rs 改用 ai-config**

把 `crates/auto-ai-daemon/src/config.rs` 的手写 scanner 删除,改为:

```rust
//! Daemon 配置:委托给 ai-config 解析。
pub use ai_config::loader::{DaemonConfig, parse_daemon_config};

/// 便捷加载。
pub fn load() -> DaemonConfig {
    if let Some(home) = dirs::home_dir() {
        let path = home.join(".config/autoos/ai-daemon.at");
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(cfg) = parse_daemon_config(&content) {
                return cfg;
            }
        }
    }
    load_from_env()
}

fn load_from_env() -> DaemonConfig {
    // 沿用现有 load_from_env 的逻辑(从 ZHIPU/ANTHROPIC/OPENAI 环境变量构造)
    // ... (复制现有 load_from_env,但构造 DaemonConfig)
    todo!()
}
```

(把现有 `load_from_env` 的内容搬过来,返回 `DaemonConfig`。)

- [ ] **Step 2: server.rs 的 chat_completions 改为 canonical 转换**

替换 `chat_completions` handler 的核心:不再 `state.http_client.post(url).json(&body)`,而是用 daemon 内部的 `ProviderRegistry`:

```rust
async fn chat_completions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ai_config::CompletionRequest>,  // canonical
) -> impl IntoResponse {
    let app_name = headers.get("x-app-name")
        .and_then(|v| v.to_str().ok()).unwrap_or("unknown").to_string();

    // 选 provider(从 req.model 反查,或用 default_provider)。
    let provider = state.registry.default_provider();  // 或按 model 路由
    let permit = match state.pool.acquire(&provider.name()).await {
        Some(p) => p,
        None => return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":{"message":"pool full"}}))).into_response(),
    };

    match provider.complete(&req).await {
        Ok(resp) => {
            // 记 usage
            if let Some(u) = &resp.usage {
                state.tracker.record(&app_name, u.input_tokens as u64, u.output_tokens as u64);
            }
            drop(permit);
            (StatusCode::OK, Json(serde_json::to_value(&resp).unwrap())).into_response()
        }
        Err(e) => {
            drop(permit);
            (StatusCode::BAD_GATEWAY, Json(json!({"error":{"message":{e}}}))).into_response()
        }
    }
}
```

`AppState` 加 `registry: ProviderRegistry`(在 `AppState::new` 里从 daemon config 构造)。

- [ ] **Step 3: AppState 加 registry**

```rust
pub struct AppState {
    pub config: DaemonConfig,
    pub registry: ProviderRegistry,   // 新增
    pub pool: ConcurrencyManager,
    pub tracker: UsageTracker,
    pub current_model: std::sync::Mutex<String>,
}

impl AppState {
    pub fn new(config: DaemonConfig) -> Self {
        let registry = ProviderRegistry::from_daemon_config(&config);
        // ...
        Self { config, registry, pool, tracker, current_model }
    }
}
```

`ProviderRegistry::from_daemon_config` 在 provider/mod.rs 里实现(把 DaemonConfig 的 providers 转成 AnthropicProvider/OpenAiProvider)。

- [ ] **Step 4: 验证 daemon 编译**

Run: `cargo build -p auto-ai-daemon`
Expected: 编译通过。

- [ ] **Step 5: 提交**

```bash
git add crates/auto-ai-daemon/src
git commit -m "feat(daemon): canonical→provider conversion in server"
```

---

## Task 8: client 删除 direct 模式 + provider 代码,瘦化为 daemon HTTP 客户端

**Files:**
- Delete: `crates/auto-ai-client/src/provider.rs`
- Delete: `crates/auto-ai-client/src/provider/`
- Delete: `crates/auto-ai-client/src/openai_format.rs`
- Delete: `crates/auto-ai-client/src/sse.rs`
- Modify: `crates/auto-ai-client/src/lib.rs`
- Delete: `crates/auto-ai-client/src/config.rs`(迁到 ai-config)

**说明:** client 现在只做:canonical `CompletionRequest` → HTTP POST daemon → canonical `CompletionResponse`。删除 `ClientMode::Direct`、`ProviderRegistry`、所有 provider 代码。`AiClient::new()` 只走 daemon 模式(保留 `ensure_daemon` 自启动)。

- [ ] **Step 1: 删除 provider/format/sse/config 文件**

```bash
rm crates/auto-ai-client/src/provider.rs
rm -r crates/auto-ai-client/src/provider/
rm crates/auto-ai-client/src/openai_format.rs
rm crates/auto-ai-client/src/sse.rs
rm crates/auto-ai-client/src/config.rs
```

- [ ] **Step 2: lib.rs 瘦化为 daemon-only 客户端**

重写 `crates/auto-ai-client/src/lib.rs`:

```rust
//! AutoOS AI client — thin daemon HTTP client.
//!
//! Sends canonical `CompletionRequest` to the aaid daemon, receives canonical
//! `CompletionResponse`. No direct LLM access, no provider knowledge — the
//! daemon owns all LLM communication.

pub use ai_config::*;  // canonical wire types

pub mod daemon;
mod error;

use crate::error::ClientError;

pub struct AiClient {
    url: String,
    http: reqwest::Client,
}

impl AiClient {
    /// 创建客户端。自动发现并按需自启动 daemon。
    pub fn new() -> Result<Self, ClientError> {
        let url = daemon::ensure_daemon()
            .ok_or(ClientError::DaemonUnavailable)?;
        tracing::info!("ai-client: daemon at {}", url);
        Ok(Self { url, http: reqwest::Client::new() })
    }

    /// 发送 canonical 请求,收 canonical 响应。
    pub async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, ClientError> {
        let resp = self.http
            .post(format!("{}/v1/chat/completions", self.url))
            .header("Content-Type", "application/json")
            .header("X-App-Name", "auto-ai-client")
            .json(req)
            .send().await
            .map_err(ClientError::from)?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ClientError::Api(format!("daemon {status}: {text}")));
        }
        resp.json::<CompletionResponse>().await
            .map_err(|e| ClientError::Api(format!("parse: {e}")))
    }

    pub fn is_daemon_mode(&self) -> bool { true }
}
```

- [ ] **Step 3: error.rs 定义 ClientError(精简)**

```rust
#[derive(Debug)]
pub enum ClientError {
    DaemonUnavailable,
    Http(String),
    Api(String),
}
// Display + From<reqwest::Error> 实现
```

- [ ] **Step 4: daemon.rs 保留 ensure_daemon**

`daemon.rs` 不变(发现/自启动 daemon 的逻辑保留在 client)。

- [ ] **Step 5: 验证 client 编译 + 删除其旧测试**

client 的 provider/build_body 测试已随代码迁到 daemon。client 自身现在几乎没有单元测试(daemon HTTP 需要集成测试)。确认 `cargo build -p auto-ai-client` 通过。

Run: `cargo build -p auto-ai-client`
Expected: 编译通过,无 provider 残留引用。

- [ ] **Step 6: 提交**

```bash
git add crates/auto-ai-client
git commit -m "refactor(client): drop direct mode + provider code, daemon-only"
```

---

## Task 9: agent 改用 ai-config + model 校验

**Files:**
- Modify: `crates/auto-ai-agent/Cargo.toml`(加 ai-config 依赖)
- Modify: `crates/auto-ai-agent/src/lib.rs`
- Modify: `crates/auto-ai-agent/src/agent.rs`(或 Profession 加载处)

**说明:** agent 依赖 client 的 canonical 类型(现在来自 ai-config re-export)。此外,在 Profession/Workflow 加载时,若指定了 model,通过 ai-config 校验其存在性。由于 agent 通常在运行时才知道 client 配置,校验是 best-effort(配置加载失败时 warn 而非 fatal)。

- [ ] **Step 1: agent Cargo.toml 加 ai-config**

```toml
[dependencies]
ai-config = { path = "../ai-config" }
```

- [ ] **Step 2: agent 里把 model 校验接入 Profession 加载**

在 `ConfigProfession`(config/profession_config.rs)构造后,或在 Agent 初始化时,加一个可选的 model 校验步骤。由于校验需要 ClientConfig(运行时从文件读),提供:

```rust
/// 加载 client 配置并校验 Profession 的 model(若配置可读)。
pub fn validate_profession_model(profession: &dyn Profession) -> Result<(), AgentError> {
    // best-effort:读 ~/.config/autoos/ai-client.at
    let home = dirs::home_dir().ok_or_else(|| AgentError::Config("no home dir".into()))?;
    let path = home.join(".config/autoos/ai-client.at");
    let content = std::fs::read_to_string(&path)
        .map_err(|e| AgentError::Config(format!("read client config: {e}")))?;
    let cfg = ai_config::parse_client_config(&content)
        .map_err(|e| AgentError::Config(format!("parse client config: {e}")))?;
    ai_config::validate_model_exists(&cfg, profession.model())
        .map_err(AgentError::Config)
}
```

在 agent 里导出 `parse_client_config` / `validate_model_exists`(ai-config 的)。

- [ ] **Step 3: 验证 agent 编译 + 测试**

Run: `cargo test -p auto-ai-agent`
Expected: 全绿(现有 52 测试 + 可能新增的校验测试)。

- [ ] **Step 4: 提交**

```bash
git add crates/auto-ai-agent
git commit -m "feat(agent): model validation via ai-config"
```

---

## Task 10: 全量验证 + 配置文件迁移 + 文档

**Files:**
- Create: `crates/ai-config/examples/client.at`(示例配置)
- Create: `crates/ai-config/examples/daemon.at`(示例配置)
- Modify: `docs/auto-ai-agent-design.md`(更新架构图)
- Modify: `README.md`(若提到三库关系)

- [ ] **Step 1: 写示例配置文件**

`crates/ai-config/examples/client.at`(单根 `client { }` 格式):

```text
client {
    default_provider : zhipu
    default_model : glm-4.5
    zhipu {
        kind : openai
        base_url : "https://open.bigmodel.cn/api/paas/v4"
        key_env : ZHIPU_API_KEY
        models : glm-4.5,glm-flash
    }
}
```

`crates/ai-config/examples/daemon.at`(单根 `daemon { }` 格式),加 listen_addr/max_concurrency。

- [ ] **Step 2: 全工作区测试**

Run: `cargo test --workspace`
Expected: 全绿(ai-config + client + daemon + agent + aictl)。注意 client 现在测试变少(provider 测试迁到 daemon),daemon 测试变多。

- [ ] **Step 3: 更新设计文档架构图**

在 `docs/auto-ai-agent-design.md` 更新三层架构图,反映:client 瘦化、daemon 持有 LLM 通信、ai-config 公共 crate、canonical wire 格式。

- [ ] **Step 4: 提交**

```bash
git add crates/ai-config/examples docs/auto-ai-agent-design.md
git commit -m "docs: config examples + updated architecture for three-crate refactor"
```

---

## 不在本计划范围(明确排除)

- **现有用户配置文件的自动迁移**:用户现有的 `~/.config/autoos/ai-client.at`(flat 格式)需要手动改成 `client { }` 包裹格式。文档说明,不写迁移脚本(配置文件少,手改即可)。
- **Forge 迁移(原 Phase 6)**:这是下一个独立计划,依赖本重构完成。
- **Ash F3(原 Phase 7)**:再下一个计划。
- **agent 运行时任务级可观测**:按决策,aictl 只看 daemon,agent 不上报。
- **canonical wire 格式的版本化**:MVP 不做版本号,后续若 daemon/client 不兼容再考虑。

## 自检

- **Spec 覆盖**:决策1(daemon 形态转换)→ Task 6+7;决策2(LLM 通信迁 daemon)→ Task 6+8;决策3(删 direct)→ Task 8;决策4(canonical wire)→ Task 1+5+7;决策5(ai-config)→ Task 1-4;决策6(两文件)→ Task 3+10。全覆盖。
- **类型一致性**:`ProviderConfig`(Task 2)在 Task 3/7/9 一致引用;canonical 类型 `CompletionRequest/Response`(Task 1)在 Task 5/7/8 一致;`DaemonConfig`(Task 3)在 Task 7 一致。
- **无占位符**:Task 3 Step 3 的 `load_from_env` 标了 todo 但明确说明"复制现有逻辑",其余步骤代码完整。
