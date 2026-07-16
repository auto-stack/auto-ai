# auto-os-config 自给自足设计：从插件宿主到本地配置工具

> **状态**：设计文档，待评审
> **日期**：2026-07-15
> **影响**：auto-os-config（重构为自给自足）、auto-musk（配置页面移回 os-config）
> **前置**：当前插件架构（远程 ESM bundle）

---

## 1. 问题陈述

auto-os-config 当前是"插件宿主"——它的侧栏模块（Roles/Skills/Agents/Auto Musk）的配置页面由 musk 后端(:8080) 提供 ESM bundle，数据 API 也由 musk serve 提供。

**问题**：
- 用户打开 os-config 时如果 musk 后端没启动，Roles/Skills 等页面报 "Failed to fetch"
- os-config 配置的是 `~/.config/autoos/` 下的文件，但必须等被配置的服务在线才能编辑——逻辑矛盾
- `auto-ai-cli /config` 要启动 3 个服务才能完整打开配置页面

**目标**：os-config 直接读写配置文件，不依赖任何远程服务的 API 或 ESM bundle。

---

## 2. 架构对比

### 2.1 当前（插件宿主）

```
auto-os-config (:17700, 纯前端 Vite)
  ├── AI Daemon → import('http://:17654/config-page.js') + fetch('/:17654/v1/config/data')
  ├── Roles     → import('http://:8080/roles-config-page.js') + fetch('/:8080/api/roles')
  ├── Skills    → import('http://:8080/skills-config-page.js') + fetch('/:8080/api/skills')
  └── Auto Musk → import('http://:8080/app-config-page.js') + fetch('/:8080/api/app-config')

需要：aaid(:17654) + musk(:8080) + os-config(:17700) 全部在线
```

### 2.2 目标（自给自足）

```
auto-os-config (:17700, 前端 + 轻量 Rust 后端)
  ├── Rust 后端 (axum :17700)
  │     ├── GET /api/daemon-config     → 读 ai-daemon.at
  │     ├── PUT /api/daemon-config     → 写 ai-daemon.at
  │     ├── GET /api/roles             → 扫描 roles/*.at + .soul.md
  │     ├── PUT /api/roles/:name       → 写 roles/:name.at + .soul.md
  │     ├── GET /api/skills            → 扫描 skills/*/SKILL.md
  │     ├── GET /api/modes             → 扫描 modes/*.at
  │     ├── GET /api/app-config/:app   → 读 apps/:app/config.at
  │     └── PUT /api/app-config/:app   → 写 apps/:app/config.at
  │
  └── Vue 前端（组件内置，不再远程 import）
        所有 .vue 文件在 os-config 自己的 src/ 里

需要：只有 os-config(:17700) 一个服务
```

---

## 3. 实施计划

### Phase 1：os-config 加 Rust 后端

**新建 `backend/` 目录**：
```
auto-os-config/
  backend/
    Cargo.toml
    src/
      main.rs         — axum server (:17701，不和 vite 冲突)
      config_api.rs   — 文件 CRUD API
  src/                — 现有 Vue 前端（不变）
  vite.config.ts      — dev proxy /api → :17701
```

API 端点（全部直接读写 `~/.config/autoos/`）：

```
GET  /api/daemon-config           读 ai-daemon.at（auto-atom 解析 → JSON）
PUT  /api/daemon-config           写 ai-daemon.at
GET  /api/roles                   扫描 roles/*.at → 列表
GET  /api/roles/:name             读单个 role + soul
PUT  /api/roles/:name             写 .at + .soul.md
DELETE /api/roles/:name           删
GET  /api/skills                  扫描 skills/*/SKILL.md
GET  /api/modes                   扫描 modes/*.at
GET  /api/app-config/:app         读 apps/:app/config.at
PUT  /api/app-config/:app         写 apps/:app/config.at
GET  /api/app-harness/:app/:kind  读 OS 级 harness + app 自建
PUT  /api/app-harness/:app/:kind/:name
```

这些 API 复用 auto-ai-agent 的 `parse_at_role` / `serialize_at_role` / `RoleRegistry` / `SkillRegistry`。

### Phase 2：前端组件内置化

把现在由 musk 提供的 4 个 ESM bundle（`roles-config-page.js` 等）**搬到 os-config 自己的 `src/views/`**：

| musk 的文件 | 搬到 os-config | 改动 |
|---|---|---|
| `musk/frontend/src/roles-config-page.vue` | `os-config/src/views/RolesView.vue` | API URL 从 `:8080` 改为 `/api/`（同源） |
| `musk/frontend/src/skills-config-page.vue` | `os-config/src/views/SkillsView.vue` | 同上 |
| `musk/frontend/src/agents-config-page.vue` | `os-config/src/views/AgentsView.vue` | 同上 |
| `musk/frontend/src/app-config-page.vue` | `os-config/src/views/AutoMuskView.vue` | 同上 |

同时移除 `useModules.ts` 里的远程 `import()` 逻辑，改为正常的 Vue 组件 import。

### Phase 3：移除插件加载机制

- `useModules.ts`：去掉 `remote` URL + `import()` 逻辑，改为直接 import 组件
- `index.html`：去掉 importmap（不再需要共享 Vue 运行时）
- `vite.config.ts`：去掉 `vue` alias + `optimizeDeps.exclude`
- `public/vendor/vue.runtime.esm-browser.js`：删除

### Phase 4：AI Daemon 配置页面

aaid 的配置页面（`config-page.vue`）也需要搬过来。但 AI Daemon 的配置 API（`/v1/config/data`）涉及**测试连接**功能（调 LLM），这个仍需要 aaid 在线。

**策略**：
- 读写 `ai-daemon.at` → os-config 自己做（文件操作）
- 测试连接 → 如果 aaid 在线则调 `:17654/v1/config/test`，不在线则禁用测试按钮 + 提示

### Phase 5：清理

- musk `frontend/` 目录：删除（配置页面已搬走，musk 不再需要提供 ESM bundle）
- musk `vite.config.ts` 的 lib 构建配置：删除
- musk `frontend-dist/` 目录：删除
- musk 不再提供 `/api/roles` / `/api/skills` 等配置 API（os-config 接管）—— 但 musk 自己的运行时代码仍需要读这些文件（通过 `RoleRegistry::load()` 直接读文件，不需要 HTTP API）

---

## 4. 收益

| 维度 | 当前 | 目标 |
|---|---|---|
| 打开 os-config 需要 | 3 个服务（aaid + musk + os-config） | 1 个（os-config） |
| 配置页面加载方式 | 远程 import ESM bundle | 本地 Vue 组件 |
| API 来源 | 散落在 aaid + musk | 统一在 os-config 后端 |
| Vue 运行时 | 共享（importmap + vendor） | 标准 Vite（无 hack） |
| 新增 app 的配置 | 需要 app 自己构建 ESM bundle | 在 os-config 加一个 view |
| 配置和运行时耦合 | 高（配置页面的 API 在被配置的服务上） | 零（os-config 只碰文件） |

---

## 5. 风险

| 风险 | 缓解 |
|---|---|
| os-config 变成 Rust + Vue 混合项目 | 复用 auto-ai-agent 的解析/序列化库，后端约 500 行 |
| musk 运行时仍需要配置数据 | musk 通过 `RoleRegistry::load()` 直接读文件，不需要 HTTP API |
| 迁移期间两套 API 并存 | 过渡期 musk API 保留，os-config 新 API 并行 |
| auto-atom 解析依赖 | os-config 后端依赖 auto-ai-agent crate（已验证可用） |

---

## 6. 范围

- ✅ os-config 加 Rust 后端（文件 CRUD）
- ✅ 4 个配置页面内置化（Roles/Skills/Agents/AutoMusk）
- ✅ 移除插件加载机制（importmap/vendor/import()）
- ✅ AI Daemon 页面（读写文件 + 可选的连接测试）
- ✅ musk frontend/ 清理
- ⏸ 不改 musk 的运行时配置读取（仍用 RoleRegistry::load() 读文件）
- ⏸ 不改 auto-ai-cli（/config 改为只启动 os-config）
