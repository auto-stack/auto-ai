# Spec：角色-模型绑定（有序候选链直接绑定 + tier 遗留轨道）

- 规范 ID：`docs/specs/auto-ai/role-model-binding.md`（SD-01，PLAN-034 建立）
- 实现：`crates/ai-config/src/wire.at` + `rust-ref/src/wire.rs`（wire）、
  `crates/auto-ai-agent/src/{role_def,roles,config/role_config,agent,compaction}.at` +
  `rust-ref/src/` 对应（角色层与请求构造）、
  `crates/auto-ai-daemon/src/server.{at,rs}`（候选链路由）
- 编辑器约定：`auto-os-config/src/editor/types.ts`（inferColumn provider 列，
  SD-02）
- 状态：随 PLAN-034 进入评审

## 1. 范围

agent 角色（Role）与 LLM 模型的绑定契约：角色文件字段、请求 wire 字段、
daemon 路由语义、降级规则与兼容性。覆盖三个 crate（ai-config /
auto-ai-agent / auto-ai-daemon）的跨层行为；编辑器侧仅约束约定面
（os-config 通用编辑器如何渲染 `models`）。

**tier 轨道整体保留**（`ModelTier`、`tier:` 请求 token、`tier_routing`
配置、`/api/enums/tiers`、TierRouter 均不退役）；本 spec 只在既有 tier
间接绑定之上增加「直接候选链」通道。退役属后续独立决策。

## 2. 角色文件字段与优先级

`role { … }` 块（`~/.config/autoos/roles/<name>.at`）的模型绑定字段：

```at
role {
    models : [
        { provider : "zhipu",    model : "glm-5.3" },
        { provider : "deepseek", model : "deepseek-v4-pro" }
    ]
    model : "glm-4.6"          # 遗留单模型 pin，仍可单独出现
    model_tier : "mid"         # 遗留 tier 声明，可与 models 共存
}
```

请求构造（`build_request`）按**三档优先级**取值：

1. `models` 非空 → 显式候选链生效（见 §3/§4）；
2. 否则 `model` 非空 → 单模型 pin（`model_chain` 为空）；
3. 否则 → `tier:<display_name 小写>` token（`model_chain` 为空）。

规则：

- `models` 与 `model_tier` 允许共存（迁移期）；序列化往返两者都保留，
  请求期 `models` 胜出。
- `models` 为有序数组，顺序即优先级（链首主选）；同一 provider 可出现
  多次（不同 model）。
- daemon **不校验**候选的 model id 是否声明于该 provider 的 models 列表
  （与具体 id 请求路径一致，透传给 provider）；provider 不在 daemon 配置
  的候选在 fallback 循环中天然跳过。
- 链非空时 `preferred_provider` 被忽略（显式序即用户意图）——该字段只影
  响 TierRouter 的候选重排，而显式链根本不进 TierRouter。
- `inherit`：用户角色未声明链时回落 builtin 的链（builtin 默认空——
  builtin 角色只声明 tier，保持跨环境可移植；用户角色用 `models` 显式覆
  盖）。`merge_over` 语义：子链**整链替换**基链（不逐项合并——顺序即语
  义，逐项合并会打乱声明优先级）。

## 3. wire 契约（ai-config）

```rust
pub struct ModelCandidate { pub provider: String, pub model: String }

pub struct CompletionRequest {
    // …既有字段…
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_chain: Vec<ModelCandidate>,
}
```

- 链非空时**必须**满足 `req.model == model_chain[0].model`（链首锚定）：
  老 daemon 反序列化忽略未知字段后按 `req.model` 服务，即优雅降级为
  「只用主模型」。agent 构造与 `CompletionRequest::with_model_chain` 均
  自动维持该不变式。
- 空链序列化时省略（wire 与 PLAN-034 前逐比特一致）；反序列化缺省为空。
- daemon 路由只看链，不看 `req.model`（除流式/日志语义外）。

## 4. daemon 路由与降级（auto-ai-daemon）

`chat_completions` 的候选链构建（按序判定）：

1. `model_chain` 非空 → 候选 = 链本身（跳过 TierRouter，不受
   `preferred_provider` 重排）；
2. `model` 以 `tier:` 开头 → TierRouter 候选链（显式 `tier_routing` 或
   从 providers 自动派生）+ 空链时 default-provider 兜底解析；
3. 否则 → 具体 id 属主扫描（单候选）。

fallback 循环语义（三来源共用，逐候选）：

- 并发池 `acquire_with_timeout` 失败 → 下一候选；
- provider 不在 registry → 下一候选（计入 last_error）；
- 可重试错误（429/超时/5xx/传输）→ 下一候选；非可重试 → 立即 502；
- quota/billing 耗尽 → **中止整链**（account 级故障，换候选只烧重试窗）；
- 全链耗尽 → 502（`all providers failed; last error: …`）；
- 流式请求：链首可获取即开始流（流开始后不换模型，与现状一致；饱和的
  非链首候选按 §5 短等后落到后续候选）。

响应回填 `model_meta`（Plan 031 契约不变）：`id`/`context_window` 跟随
**实际服务**的候选；配置未声明 context_window 时为 `null`（不猜窗口）。

## 5. 并发等待策略（唯一链相关行为改动）

`permit_wait(is_explicit_chain, idx, total)`：

- 显式链的**非末位**候选：`CHAIN_SHORT_WAIT = 1s`；
- 其余一切（显式链末位、tier、具体 id）：30s（现状不变）。

理由：用户声明链即表达「并发不够就用备用」；在非末位候选上等 30s 违背
该意图。末位无路可退，宁可等。`CHAIN_SHORT_WAIT` 为 server 模块常量；
如需可配置再立项。

## 6. 兼容矩阵

| 场景 | 行为 |
|---|---|
| 老 daemon 收到带 `model_chain` 的请求 | serde 忽略未知字段；按 `req.model`（=链首）服务；不报错 |
| 新 daemon 收到无 `model_chain` 的请求 | 原 tier/pin 路径逐比特不变（`Vec::is_empty` 分支不触发） |
| 既有用户角色（仅 `model_tier`/`model`） | 请求逐比特不变（AC-03 回归例锁定） |
| builtin 角色 | 继续只声明 tier，无链（`models()` 默认空） |
| 老客户端/老 UI | RoleSummary 新增 `models` 字段为加法，未知键忽略 |
| auto-musk roles-config-page | 新字段加法不破坏；UI 暂不展示（已登记 KNOWN-DEBT，待后续小计划适配） |

## 7. 测试锚点

- wire：ai-config serde 往返/空链省略/旧字段集容忍（`wire.rs` tests）。
- agent：`mvp_harness.rs`（链头锚定、全链上 wire、链胜过 pin+tier、无链
  回归、压缩携带链）；a2r `transpiled_harness.rs` T34 系列镜像。
- daemon：`server.rs` tests（链序降级+meta 跟随、无效 provider 跳过、全
  链 502、preferred 让位、池饱和 1s 快速切换、`permit_wait` 选择矩阵、
  空链/具体 id 回归）。
- 编辑器：`auto-os-config/test-plan034-models.mjs`（隔离根浏览器 e2e：
  表格渲染、provider 列 select 喂 ai-daemon 枚举、保存往返、重开显编辑）。
