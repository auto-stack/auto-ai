# Plan 032: a2r 语言能力缺陷根因修复清单——auto-lang 侧交付需求（消费端汇总）

> **状态**：🔧 进行中（2026-08-25 首批落地：**G1 ✅ + G2.1 ✅**，见各节 ✅ 标记与「实施记录」；G2.2-2.4 / G3-G8 待续）
> **实施记录（2026-08-25，auto-lang 分支 fix/a2r-p0-gaps，合入 70ed43575）**：
> 1. **G1 ✅**：`infer/expr.rs` 对 comptime `#{read_text/read_to_string/include_str}` 推断为 StrSlice——`const SOUL: &str` 原生发射。golden：16_interop/017 扩 const 用例。
> 2. **G2.1 ✅（性质修正 + 双侧修复）**：`mut p T` 的 `&mut` 渲染是 auto-lang Plan 018 C11 的**有意 in-out 设计而非缺陷**（消费端语义本应按值）。修复：① auto-ai `pipeline.at` 的 `correct_handoff_target` 去掉两个参数的 `mut`（rust-ref 原版按值、调用方传 clone，语义无损）；② auto-lang `trans/rust.rs` 参数 mut 后处理扫描新增「自有非基本类型参数的任意方法调用 → 保守自动 `mut`」（自定义方法如 `push_gate_feedback` 无法在转译期判定 &mut self）。golden：02_types/011_param_auto_mut。
> 3. **消费端 sed 毕业**：agent `retranspile.sh` 的 SOUL 段与 handoff 段验证 no-op 后替换为 GRADUATED 注释删除；另收窄 compact() 调用 sed 锚点（master a2r 已原生发 `model.as_str()`，旧锚点失配）。
> 4. **验证**：auto-lang 3172 单测 + golden 套件（含 2 个新用例）全绿；auto-ai 转译轨 24 测试全绿 + 重生成后编译干净。注意：本次重转译引入 master a2r 的正常漂移（`push(x)` → `push(x.to_string())` 等），随批接受。
> **仓库**：auto-ai（本文档；零代码改动）/ auto-lang（修复实施方）
> **背景**：KNOWN-DEBT-AND-RISKS.md 中十余行债务同根因——a2r（Auto→Rust 转译器）语言能力缺口。它们目前分散在各 crate 的 `retranspile.sh` sed 段与 4 个手写 glue 文件里，每行各自记录、视角零碎。本计划把它们收拢成一份按缺陷类别组织的清单，供 auto-lang 统一立项、分批修复。
> **产出**：修复完成后，本文档的验收节逐项勾销，KNOWN-DEBT 对应行核销。

## 0. 现状盘点（2026-08-25 实测）

| 消费端 | workaround 载体 | 规模 |
|---|---|---|
| auto-ai-agent | `retranspile.sh` | 2 条独立 sed（SOUL const 类型、correct_handoff_target 借用反转）+ handoff.at `routes`→`entries` 保留字规避 |
| auto-ai-client | `retranspile.sh` | 8 条 sed（其中部分为永久性重写，见 G4） |
| ai-config | `retranspile.sh` | 2 条 sed（永久性重写） |
| auto-ai-daemon | `retranspile.sh` | **160 行 sed**（turbofish、match 绑定 deref、owned field move、`.`→`::` 路径、Plan 019 D 类、extern-crate shim 等） |
| auto-ai-daemon 手写 | `provider_glue.rs` 436 行 / `server_glue.rs` 310 行 / `services.rs` 209 行 / `main.rs` 87 行 / `tier_router_glue.rs` | 合计 1000+ 行非转译代码 |

## 1. 缺陷清单（按类别）

### G1 类型推断：comptime 字符串字面量推断为 `/* unknown */`（P0）

- **现象**：comptime `read_text` 产字符串字面量，a2r 推断不出类型，发射 `const SOUL: /* unknown */`。
- **workaround**：agent sed 改 `&str`（retranspile.sh L155-159，Plan 016 3.1）。
- **根因**：a2r 对 comptime 求值结果的类型不回填到 const 声明。
- **修复判据**：agent 侧该 sed 段重生成后 no-op。

### G2 借用/可变性渲染（P0，已有多起同类先例修复）

a2r 借用推理是历史重灾区（Plan 396 修了 B/C/D/E 四类），现存残余：

1. **`mut` 参数渲染成 `&mut`**：`fn f(mut eng: PipelineEngine)` 被发射为 `&mut PipelineEngine`——借用反转（Plan 387 §16 aftermath，agent sed L178-179）。
2. **match 绑定不 deref**：match 臂绑定需要 `.as_str()`/clone 补偿（daemon sed）。
3. **owned field 被 move**：方法内把字段传给自由函数报 E0507，需手工 borrow（daemon sed：`str_find(self.buf, ...)`）。
4. **`&String` 入集合前 clone**：push 前需显式 clone（daemon sed）。

### G3 路径与命名（P0）

1. **成员访问 `.` 误发模块路径**：应 `use a::b` 却发 `use a.b`，需 sed `.`→`::`（daemon、provider_glue/tier_router_glue 同类）。
2. **extern-crate 引用**：a2r 发射 `use crate::ai_config::...`，跨 crate 符号解析不了，靠 extern-crate shim 垫（daemon retranspile 首段）。
3. **`.at` 保留字撞 Rust 惯用名**：`routes` 是 .at 保留字，tier_router 的 `routes` 字段只能经手写 `tier_router_glue.rs` 暴露；handoff.at 被迫改名 `entries`。需要保留字转义机制（如 `` `routes` `` 反引号或字段名映射）。

### G4 类型系统细节（P1，部分判定为永久重写）

turbofish 显式标注、`as u32` 窄化、`as_array` 解包、`JsonValue` 别名注入、mut stream——client 7 条 + ai-config 2 条 sed 中，**账本已判定其中若干为「永久性重写」**（.at 侧可控写法或胶水层语义，而非根因可消除项）。本类修复前需先逐条重判性质：可根除的根除，永久的从「债务」改记「转正写法」并从 sed 移入 .at 源码。

### G5 宏与运行时入口（P1）

1. `println`/`eprintln` 渲染为函数调用（应为宏 `println!`）。
2. `env.args()` 路由到不存在的 `a2r_std::env::args`。
3. `#[tokio::main]` 双重输出。

→ 后果：daemon `main.rs`（87 行）整体手写。修齐这三项后 main 可回迁 .at。

### G6 async/并发原语（P2，大工程）

1. **`tokio::select!` 无 .at 语法**——provider 的取消响应 + idle-timeout 竞速循环留 436 行 `provider_glue.rs`（openai/anthropic complete_stream）。需要 biased 竞速语义的表达。
2. **`impl Drop` 无 .at 语法**——`CancelOnDrop`（RAII 断连取消）留 server_glue。
3. **裸 `tokio::spawn` + 双向 mpsc + `async_stream::stream!`**——SSE 流式桥接整体留 server_glue（310 行）。actor 抽象之外的并发三件套无转译先例。

### G7 生态胶水（P2，部分永久手写）

- `cfg!(windows)`、`std::process::Command` 链、`reqwest::blocking`、`spawn_blocking` → `services.rs` 209 行直接复制，**转译风险高、建议永久手写**（账本 025 行已此判定）。
- tower_http 层（ServeDir/CorsLayer）、`env!`/proc-macro、route_service 挂载 → `server_glue.rs` build_router。
- **builder 链断裂**：a2r-std 的 `send_async()` 无法经方法链转译调用（Plan 024 附注）——方法链转译需支持。

### G8 trait/spec 语义（P2）

spec 默认体不能调 `self`（DIV-TRAIT-A2R-3，Plan 022 时以「无默认体」绕过）；spec 内泛型方法 + `'static` 大工程（账本 022 行已文档化为长期限制，`*_shared` workaround 可用）。

## 2. 优先级与批次建议

| 批次 | 类别 | 理由 |
|---|---|---|
| P0 | G1 + G2 + G3 | 同类根因已有修复先例（Plan 396 模式），收益最大：agent 2 sed 全消、daemon 160 行 sed 大部消解 |
| P1 | G4 重判 + G5 | G4 需逐条定性（根除 vs 转正）；G5 修齐即收回 daemon main.rs |
| P2 | G6 + G7 + G8 | 大工程或永久手写；G6.1（select!）价值最高（436 行 provider_glue 回迁），建议单独立项 |

## 3. 验收方式（消费端可自动判定）

1. **sed 毕业判定**：修复后跑各 `retranspile.sh`，对应 sed 段验证 no-op（重生成前后 rust/src 逐字节一致）后整段删除——沿用 Plan 396/await-drop 毕业的既有流程（GRADUATED 注释 → 后续删除）。
2. **glue 回迁判定**：某类修复落地后，对应 glue 文件的功能可由 .at 表达并转译通过、独立 crate 测试绿，才允许回迁；一次一类，不合并。
3. **回归底线**：每批修复后 auto-ai 侧 `cargo test --workspace`（236+）+ 转译轨独立 crate 测试（24+）+ `scripts/e2e-daemon-a2r.sh` 全绿。
4. **账本核销**：KNOWN-DEBT 对应行改「已修复」并注明 auto-lang 修复出处。

## 4. 明确不做

- `services.rs`（OS 胶水）转译——账本已判定永久手写，本计划不改判。
- G8 的 spec 泛型 `'static` 大工程——长期限制，等真实需求。
- auto-musk 侧消费缺口（PLAN-039/040 范畴）。

## 5. 对应债务行索引（修复后逐行核销）

| 债务行 | 缺陷类 |
|---|---|
| agent 2 条 sed（SOUL / handoff 借用） | G1 / G2.1 |
| agent handoff.at `routes`→`entries` | G3.3 |
| 020 client/ai-config sed 残留 | G4 |
| 025 select! → provider_glue | G6.1 |
| 025 impl Drop / streaming_response → server_glue | G6.2 / G6.3 |
| 025 main.rs 手写 | G5 |
| 025 services.rs 复制 | G7（永久，仅改记性质） |
| 025 server_glue wiring | G7 |
| 022 spec 默认体 / 泛型 `'static` | G8 |
