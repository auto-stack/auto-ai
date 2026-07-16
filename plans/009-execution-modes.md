# Plan 009: auto-ai-cli 三种执行模式 + 自动路由

> **状态**：已批准，实施中
> **仓库**：`auto-ai`（auto-ai-cli + auto-ai-agent）
> **前置**：Plan 007（auto-ai-cli）、Plan 008（orchestration 下沉）

## 4 个 Phase
1. 改写 assistant soul（路由规则）
2. spawn_pipeline 工具
3. 内置 flow + AgentFactory
4. chat 集成 + --mode
