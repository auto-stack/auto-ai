# Review 004 — 转译版 agent e2e 对照报告（Plan 022 Phase 2）

> **日期**：2026-08-07
> **计划**：Plan 022 Phase 2 — Live E2E Runner
> **目标**：验证转译版 agent（`auto-ai-react` / `auto_ai_agent_a2r`）与原生版
> （`auto-ai-cli` / rust-ref agent）在真实 daemon + LLM 全链路下的行为一致性。

---

## 1. 验证设施（已交付）

### 1.1 e2e runner 脚本
**`scripts/e2e-transpiled.sh`** — 完整的端到端验证 runner：
- 前置检查（API key 存在）→ 构建二进制 → 拉起 `aaid` daemon → 轮询 `/v1/status` 等待就绪
- 跑转译版 `auto-ai-react` 两条路径：
  - **纯文本路径**（"Say exactly: hello world"）→ 断言输出含 "hello world"
  - **工具调用路径**（"Use the echo tool to echo: hello"）→ 断言输出含 echo/hello 痕迹
- `trap cleanup` 保证 daemon 进程清理；daemon 日志保留用于诊断
- 无 API key 时正确以退出码 1 报错（已验证）

### 1.2 live 集成测试
**`crates/auto-ai-agent/rust/tests/live_run.rs`** — 两个 `#[ignore]` 测试：
- `live_transpiled_react_one_turn`：用 `Assistant` role + `EchoTool`，跑一轮真实对话，断言 turns≥1 + output 非空
- `live_transpiled_react_tool_call`：自定义 `EchoRole`（转译版 API），验证工具调用路径
- daemon 不可达时 soft-skip（对标 `rust-ref/tests/live_run.rs` 形态）
- 编译通过（`cargo test --no-run` 0 错）

### 1.3 转译版离线 mock 套件（Phase 1，已全绿）
**`crates/auto-ai-agent/rust/tests/transpiled_harness.rs`** — 14 测试全绿，覆盖
ReAct 循环 / Tool 注册 / Skill 工具 / 内置角色 / 流式事件 / preferred_provider。
**这是转译版的第一个自动化测试体系**，对标 rust-ref `mvp_harness.rs`。

---

## 2. 运行链路确认（调研结论）

```
auto-ai-react 二进制（转译版）
  → 转译版 Agent / ReAct 循环（rust/src/agent.rs）       ✅ Phase 1 mock 覆盖
  → 手写胶水 StreamingAiClient（rust/src/client_impl.rs） ✅ 桥接层
  → 真实 rust-ref AiClient（131+ 测试覆盖）                ✅ 非 Plan 022 范围
  → HTTP → aaid daemon → LLM 提供商                        ⬜ 需 API key 实跑
```

**关键事实**：转译版 `auto-ai-client/rust/` 和 `ai-config/rust/` 是死代码（不在运行链路）。
转译版 agent 通过 `client_impl.rs` 直接用真实 rust-ref 的 `AiClient`/`ai-config`。
因此 e2e 验证的主体是**转译版 agent 层**，client/config 层已由 rust-ref 测试覆盖。

---

## 3. Live e2e 实际运行状态

### ⬜ 本次未实跑（环境无 API key）

当前环境未设置 `ZHIPU_API_KEY` / `OPENAI_API_KEY` / `ANTHROPIC_API_KEY`，无法实际运行
live e2e。这是预期内的——计划已明确 live 测试需手动触发 + API key，不进默认 CI。

### 运行方式（用户具备 API key 时）

```bash
# 方式一：e2e runner 脚本（推荐，自动拉起 daemon + 断言）
export ZHIPU_API_KEY=sk-...
bash scripts/e2e-transpiled.sh

# 方式二：live 集成测试（手动触发 ignored 测试）
# 先单独启动 daemon：
cargo run -p auto-ai-daemon &
# 再跑转译版 live 测试：
cargo test --manifest-path crates/auto-ai-agent/rust/Cargo.toml \
  --test live_run -- --ignored
```

### 双轨对照方法（验证行为一致性）

同一 prompt 分别跑两个版本，对比**输出结构**（非逐字——LLM 有随机性）：
- 转译版：`auto-ai-react`（rust/src/main.rs）
- 原生版：`auto-ai-cli run <task>`（用 rust-ref agent）

对比维度：
1. 工具是否被调用（tool_calls 计数）
2. 轮数（turns）
3. stop_reason
4. token 用量量级

用确定性 prompt（"Say exactly: ..."）降低方差。

---

## 4. 已验证的行为等价点（离线 mock，Phase 1）

以下行为等价性已由 14 个离线 mock 测试**自动验证**（不依赖 LLM）：

| 行为 | 转译版测试 | rust-ref 对标 |
|---|---|---|
| 工具调用→反馈→结束 | T1 | agent.rs `run_tool_then_finish` |
| 纯文本单轮对话 | T2 | agent.rs `run_plain_text` |
| hard turn cap（max_turns×5） | T3 | agent.rs `run_exceeds_hard_turn_cap` |
| client 错误传播 | T4 | driver.rs ErrorClient 场景 |
| 多轮工具链 | T5 | agent.rs 多轮测试 |
| register_tool 可调用 | T6 | tool.rs 注册测试 |
| register_shared 等价 | T7 | tool.rs 注册测试 |
| 未注册工具→错误消息 | T8 | tool.rs 未注册测试 |
| register_skill_tool 注册工具（Phase 3.1 修复） | T9 | agent.rs register_skill_tool |
| 内置角色加载 + 14 个完整 | T10 | builtin_roles 测试 |
| 内置角色真实 souls | T11 | mvp_harness 灵魂检查 |
| run_stream 发 Done 事件 | T12 | driver.rs 流式事件测试 |
| run_stream 响应 cancel | T13 | driver.rs cancel 测试 |
| preferred_provider 传播（Phase 3.2 修复） | T14 | （rust-ref 无独立测试，行为对齐） |

---

## 5. 待 live 验证项（需 API key）

以下只能由 live e2e 验证（mock 测不到真实 HTTP/SSE/daemon 协议）：

- [ ] 真实 SSE token 流经 StreamingAiClient 侧信道显示（main.rs 的 printer thread）
- [ ] daemon 真实并发 / 用量统计在转译版路径下正常
- [ ] 真实工具调用（EchoTool）的 end-to-end 往返
- [ ] 转译版 vs 原生版输出结构对照（双轨）

**结论**：设施就绪，待用户具备 API key 时执行 `scripts/e2e-transpiled.sh` 完成验证。
本 review 将在 live e2e 实跑后补充实际对照数据。
