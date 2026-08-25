# Plan 032: a2r 语言能力缺陷根因修复清单——auto-lang 侧交付需求（消费端汇总）

## Status: COMPLETE（首阶段收口 + 合并后复验收官，2026-08-25）

G1/G2.1/G2.2/G2 核心首批/G3.1/G4 全部落地;daemon sed 160→80、client 8→5、agent 2→0;修复全部合入 auto-lang master(plan-032-g2-get-borrow 合并 c89253b18 + Phase 4 缺口修复 c3cb3dd4f)。残余主题(G2 核心其余族 / G3.2 extern-crate 跨 crate 类型知识 / G3.3 保留字转义)已在第 10 条定性移交后续立项,本计划按批维护模式随归档关闭——后续批次直接在 auto-lang 新计划开号,不再回写本文档。

> **状态**：⏸ 首阶段收口（2026-08-25 七批落地 + 合并后复验：**G1 ✅ + G2.1 ✅ + G2.2 首滴 ✅ + G2 核心 get 引用族首批 ✅ + G3.1 ✅ + G4（Value 构造 + u32 强转）✅**；sed 蚕食 daemon 160→96→87→**80**、client 8→5、agent 2→0；`plan-032-g2-get-borrow` 已 rebase 至 master 并合入（c89253b18），合并后复验暴露 plan 016 Phase 4 三处试点缺口、当批修复入 master（c3cb3dd4f + 守卫 golden eb7abd0cb）；剩余收敛为 G2 核心其余族 / G3.2 两个架构主题 + G3.3 语言设计，本计划按批维护模式）
> **实施记录（2026-08-25，auto-lang 分支 fix/a2r-p0-gaps，合入 70ed43575）**：
> 1. **G1 ✅**：`infer/expr.rs` 对 comptime `#{read_text/read_to_string/include_str}` 推断为 StrSlice——`const SOUL: &str` 原生发射。golden：16_interop/017 扩 const 用例。
> 2. **G2.1 ✅（性质修正 + 双侧修复）**：`mut p T` 的 `&mut` 渲染是 auto-lang Plan 018 C11 的**有意 in-out 设计而非缺陷**（消费端语义本应按值）。修复：① auto-ai `pipeline.at` 的 `correct_handoff_target` 去掉两个参数的 `mut`（rust-ref 原版按值、调用方传 clone，语义无损）；② auto-lang `trans/rust.rs` 参数 mut 后处理扫描新增「自有非基本类型参数的任意方法调用 → 保守自动 `mut`」（自定义方法如 `push_gate_feedback` 无法在转译期判定 &mut self）。golden：02_types/011_param_auto_mut。
> 3. **消费端 sed 毕业**：agent `retranspile.sh` 的 SOUL 段与 handoff 段验证 no-op 后替换为 GRADUATED 注释删除；另收窄 compact() 调用 sed 锚点（master a2r 已原生发 `model.as_str()`，旧锚点失配）。
> 4. **验证**：auto-lang 3172 单测 + golden 套件（含 2 个新用例）全绿；auto-ai 转译轨 24 测试全绿 + 重生成后编译干净。注意：本次重转译引入 master a2r 的正常漂移（`push(x)` → `push(x.to_string())` 等），随批接受。
> 5. **G3.1 ✅（第二批，消费端源码规范化 + golden 钉桩）**：路径类缺陷**根因不在 a2r**——`use.rust X` 声明后模块限定调用 `X.fn()` 本就原生发 `X::fn()`（tier_router.at 是正确范例），daemon 的 8 条路径 sed 全因 .at 源码漏声明：config.at 补 `use.rust dirs`；provider/openai/anthropic.at 补 `use.rust crate::provider_glue`；`use sse: SseParser` 改 `use.rust crate::sse::SseParser`；tier_router.at 改 `use.rust crate::tier_router_glue`。daemon sed 160→152，live e2e（转译 daemon 全链路 hello world + TurnStart 断言）通过。auto-lang 侧新增 golden 16_interop/018（钉住该行为防回归）。**G3.2 extern-crate shim 与 G3.3 `routes` 保留字仍待续**（前者是单文件转译缺项目上下文的架构问题，后者需 .at 保留字转义机制）。
> 6. **master a2r 漂移暴露的回归**：master 停止在调用点对结构体参数自动 `.clone()`（tier_router 的 `base` 被移动两次）——按 G4 哲学在 .at 源码显式写 `base.clone()` 修复；`.await` 两处保留为 sed（手写 glue 函数的异步性对 a2r 不可知）。
> 7. **第三批（同日）：daemon sed 活性审计**——用「禁全部 sed 生成原始输出 + 真 sed 按序回放逐条检测」的方法论（python 仿真 sed 不可靠，弃用），对 151 条 sed 逐条判定：**125 活 / 21 死 / 5 函数体**。21 条死行（master a2r 演进后的 no-op：`&t` 实参、entry 元组 clone、usize 推断等）删除，`as u32 as u32` 双重应用顺带修正。daemon sed **152 → 131**。验证：重生成输出与 HEAD 仅差该双重应用一行（语义等价）、编译零错误、live e2e 通过。剩余 131 条（125+5 函数体+1 毕业注释误差）均为活 sed，属 G2.2-2.4/G4 借用与类型根因批次。
> 8. **第四批（同日）：G4 serde_json::Value 构造适配（auto-lang `trans/rust.rs` call() 顶端）**——外部枚举 `Value.String(x)/Value.Number(n)` 变体持有 OWNED 值，通用分支不知情：String → 实参 `.to_string()`；Number → `serde_json::Number::from`（整型/整型绑定）或 `from_f64(...).unwrap_or(...)`（浮点字面量/float 类型绑定，经 local_var_types 判定）。golden 16_interop/019 钉桩（四形态）。daemon 35 条 Value 类 sed 毕业，**131 → 102**。**残留**：`temperature` 的 `t` 绑定来自对外部类型字段的 match——浮点性转译期不可知，`from(t)→from_f64(t)` 两条窄化 sed 保留（G2 类合理残留）。定位教训：`call()` 是 4000 行函数且有多个先返回分支，适配逻辑放函数顶端才可靠（tag 分支/module.Type 分支都不会截胡）。live e2e 首跑失败为模型未按 prompt 复述（偶发），复跑全绿。
> 9. **第五批（同日）：G4b usage-token `as u32` 强转（双侧）**——① a2r `struct_init` 对 `uint` 字段喂 i64 表达式（整型字面量/i64 局部/`json.as_int`）原生补 `as u32`（golden 02_types/012 钉桩）；② daemon/client 的 `Usage` 是 **ai-config 跨 crate 外部类型**（`struct_field_types` 无条目，a2r 单文件模式不可知——G3.2 的又一例），该 11 处按转正哲学在 .at 源码显式写 `.as(uint)`。daemon 6 条 + client 2 条 sed 毕业：**daemon 102 → 96，client 8 → 6**。agent 轨同步重生成，24 测试绿；三转译 crate 编译零错；live e2e 4✅。
> 10. **第六批（同日）：G2.2 首滴 + 残余定性收口**——a2r 四处 mutating-method 扫描白名单补 `next`（迭代器推进的绑定原生 `let mut`，golden 07_ownership/025），client `let mut stream` sed 毕业（**8 → 5**）。**残余定性（本计划 sed 蚕食阶段收口）**：daemon 96 条经变换类型分组为长尾（杂项 61 / 签名 15 / clone 7 / as_str 5 / cast 3 / path 3 等），无超过 6 条的同类——继续蚕食的边际收益归零；client 5 条中 1 条永久（组装层 artifact，G3.2）、4 条 G2 借用类；ai-config 2 条（JsonValue 别名注入——候选特性「use.rust as 支持」；tier clone——G2 核心）。**结论：剩余 sed 的根因收敛为 G2 核心（借用返回类型/迭代引用的系统类型推断）与 G3.2（跨 crate 类型知识）两个架构级主题，建议在 auto-lang 按其 Plan 396 模式独立立项，本计划转入按批维护模式。** G3.3 保留字转义经查与 backtick f-string 原始字符串语法冲突，转义符选择需要正式语言设计，一并留给 auto-lang 立项。
> 11. **第七批（同日）：G2 核心 get 引用族首批（auto-lang `plan-032-g2-get-borrow` 分支，`.worktree/plan-032` worktree 实施）**——a2r 系统修复 `.get()` 返回 `Option<&V>` 的消费链：① get key 借用白名单扩 User/Tag/Enum/GenericInstance 键（`K: Borrow<K>` 恒成立故借用恒安全；Int/Uint 仍排除——Vec::get 取 usize 值）；② 新 `get_ref_bindings` 集（is 臂按臂注册/恢复，两发射点 is_stmt/Expr::Is 覆盖）：`Some(x)` 绑定的 `return x` / `return Ok(x)` / `return Some(x)` 与 let/var 初始化自动 `.clone()`；③ 语句位 is 混合臂归一：多语句块臂旁的裸**方法调用**表达式臂（`insert` 返 Option）包 `{ expr; }`（println/字面量等 unit 形态不包，golden 零漂移）。golden 07_ownership/026 五形态钉桩；auto-lang a2r golden 342 过/12 败与 master 基线（e1af10d1a wip 遗留）逐条一致零新增。消费端 daemon **9 条 sed 毕业（96 → 87）**：tier_router 6（get 借用 ×2 / return v.clone / let-init clone / 混合臂包裹）+ tracker 1（return entry.clone）+ provider 2（Ok(p)/Some(p) clone）；`return Some(c)` 属借用循环变量族（借 Plan 399 §11.4 处理器查 Expr::Call 形态、而 `Some(c)` 实为 Expr::Some 从未触发的死分支教训），经 .at 显式 `c.clone()` 转正。毕业证明：删 sed 后重生成与全量 sed 输出**逐字节一致**。同批重生成三转译 crate，漂移均为字面量 `as u32` 与 G2.1 auto-mut 等语义等价形态（master a2r 演进，随批接受）；workspace 236 测试 + 转译轨 24 测试全绿 + live e2e 4✅（转译版 agent→client→daemon→LLM 全链路）。**方法论沉淀：`Some(x)` 在 .at 解析为 Expr::Some 专用节点而非 Call——凡按 `Expr::Call(Ident("Some"/"Ok"))` 形态写的处理器都是死代码（Plan 399 §11.4 同病）。**
> 12. **第七批合并后复验（同日，auto-ai 侧第二批提交）**——`plan-032-g2-get-borrow` rebase 至 master（06d086abc plan 016 Phase 4 已入）后合入（c89253b18），golden 026 期望对齐 Phase 4 的 `vec![tc.clone()]` 防 move 改进。**合并后重生成暴露 Phase 4 三处试点缺口**（其试点仅 autodown-core crate，无 auto-ai 形态）：① 裸 `x.trim()` 族实参被误加 `.as_str()`（expr_contains_string 的 Phase 4 扩表被借用判定复用，trim 本产 `&str` → E0658，agent skill.at）；② 未知被调者 `.as_str()` 兜底叠在 .at 显式 `X.to_string()` 上经后处理坍缩成 `X.as_str()`，反转作者物化 owned String 的意图（fn-pointer 字段 `fn(String)` → E0308，agent driver.at）；③ Phase 4 调用点防 move clone 与 daemon 侧「fn 签名改 &Vec」类 sed 反向打架（`blocks.clone()` 传给 sed 改出的 `&Vec` 形参 → E0308）。**修复**：①② 在 auto-lang master 外科修复（c3cb3dd4f，守卫 golden 04_strings/009 钉桩 eb7abd0cb）；③ 按 Phase 4 已保证调用点 clone 的事实毕业 daemon 反向 sed **7 条（87 → 80）**：format.rs 组 4 条（all_tool_results/has_tool_use 调用借用 + 双签名改 &Vec）+ 其配套 awk（`for b in &blocks` 修正）+ anthropic.rs 组 3 条（content_blocks_to_anthropic 调用借用 + 签名 + 全局 `for b in &blocks`）——按值形参 + 调用点 clone 原生可编译。终局验证：四转译 crate 编译零错、workspace 236 + 转译轨 24 测试全绿、live e2e 4✅。**教训：并行批次的 codegen 改进与消费端 sed 是隐式耦合——每次 auto-lang 侧合流后必须立即重生成全部消费端核对 sed 存活状态（本批「&Vec 签名改写」类 sed 从「必要」变「有害」正是此耦合的实证）。**
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

### G3 路径与命名（P0）→ **G3.1 ✅ 毕业（消费端源码规范化）**

> **2026-08-25 结论**：G3.1（`.`→`::`）不是 a2r 缺陷——`use.rust X` 声明后模块限定调用原生发 `::`（daemon 8 条路径 sed 已因 .at 补声明毕业删除，golden 16_interop/018 钉桩）。裸 `use.rust X` 发 `use X;` 不带 `crate::`，本地手写模块需显式写 `use.rust crate::X`。**剩余**：

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
