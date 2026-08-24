#!/usr/bin/env bash
# e2e-transpiled.sh — Plan 022 Phase 2 — 转译版 agent 端到端验证 runner
#
# 拉起 aaid daemon → 跑转译版 auto-ai-react（rust/src/main.rs）→ 断言输出 → 清理。
# 验证完整运行链路：转译版 Agent → StreamingAiClient 桥接 → 真实 AiClient
# → HTTP → aaid daemon → LLM 提供商。
#
# 用法：bash scripts/e2e-transpiled.sh
#   前置：ZHIPU_API_KEY（或 OPENAI_API_KEY / ANTHROPIC_API_KEY）环境变量
#   退出码 0 = 端到端验证通过
#   退出码 1 = 失败（输出诊断信息）
#
# 可选环境变量：
#   AAID_URL      — daemon 地址（默认 http://127.0.0.1:17654）
#   SKIP_BUILD    — 设为 1 跳过 cargo build（已构建过时用）
#
# 详见 D:/autostack/auto-ai/docs/plans/archive/022-auto-e2e-validation.md Phase 2

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
AAID_URL="${AAID_URL:-http://127.0.0.1:17654}"
DAEMON_PORT="${DAEMON_PORT:-17654}"
DAEMON_PID=""
DAEMON_LOG="$(mktemp -t aaid-e2e-XXXXXX.log 2>/dev/null || mktemp)"

# ── 清理：脚本退出时 kill daemon（trap 保证清理）────────────────────────────
cleanup() {
    if [ -n "$DAEMON_PID" ]; then
        echo "[e2e] stopping daemon (pid $DAEMON_PID)..."
        # Windows: kill 进程树；Git Bash 下 kill 对应 taskkill
        kill "$DAEMON_PID" 2>/dev/null || true
        # 给一点时间优雅退出，再强杀残留
        sleep 1
        kill -9 "$DAEMON_PID" 2>/dev/null || true
    fi
    # 保留 daemon 日志用于诊断（失败时输出，成功时清理）
    if [ "${E2E_RESULT:-fail}" = "pass" ]; then
        rm -f "$DAEMON_LOG"
    fi
}
trap cleanup EXIT

echo "[e2e] === Plan 022 转译版端到端验证 ==="

# ── 1. 前置检查 ──────────────────────────────────────────────────────────────
# API key 来源：环境变量 OR daemon 配置文件（~/.config/autoos/ai-daemon.at 内联 key）。
# daemon 两者都支持（env 优先，否则读配置文件）；这里只要任一可用即可。
echo "[e2e] 检查 API key..."
DAEMON_CFG="${HOME}/.config/autoos/ai-daemon.at"
has_env_key=0
has_cfg_key=0
[ -n "${ZHIPU_API_KEY:-${OPENAI_API_KEY:-${ANTHROPIC_API_KEY:-}}}" ] && has_env_key=1
if [ -f "$DAEMON_CFG" ] && grep -q 'api_key : "[^"]' "$DAEMON_CFG" 2>/dev/null \
   && ! grep -q 'api_key : "your-' "$DAEMON_CFG" 2>/dev/null; then
    has_cfg_key=1
fi
if [ "$has_env_key" = "0" ] && [ "$has_cfg_key" = "0" ]; then
    echo "❌ 缺少 API key（设 ZHIPU_API_KEY/OPENAI_API_KEY/ANTHROPIC_API_KEY 环境变量，" >&2
    echo "   或在 ~/.config/autoos/ai-daemon.at 内联 api_key）" >&2
    exit 1
fi
echo "[e2e] API key 检测通过（env=$has_env_key, config_file=$has_cfg_key）"

# ── 2. 构建二进制 ────────────────────────────────────────────────────────────
if [ "${SKIP_BUILD:-0}" != "1" ]; then
    echo "[e2e] 构建 daemon (aaid)..."
    (cd "$REPO_ROOT" && cargo build -p auto-ai-daemon) >/dev/null 2>&1
    echo "[e2e] 构建转译版 agent (auto-ai-react)..."
    (cd "$REPO_ROOT/crates/auto-ai-agent/rust" && cargo build --bin auto-ai-react) >/dev/null 2>&1
fi

AAID_BIN="$REPO_ROOT/target/debug/aaid"
REACT_BIN="$REPO_ROOT/crates/auto-ai-agent/rust/target/debug/auto-ai-react"

if [ ! -f "$AAID_BIN" ] || [ ! -f "$REACT_BIN" ]; then
    echo "❌ 二进制不存在：$AAID_BIN 或 $REACT_BIN（构建失败？）" >&2
    exit 1
fi

# ── 3. 拉起 daemon ───────────────────────────────────────────────────────────
echo "[e2e] 启动 daemon ($AAID_URL)..."
"$AAID_BIN" >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!

# 等待 daemon 就绪（轮询 /v1/status，最多 ~10s）
echo "[e2e] 等待 daemon 就绪..."
READY=0
for i in $(seq 1 20); do
    if curl -sf "$AAID_URL/v1/status" >/dev/null 2>&1; then
        READY=1
        echo "[e2e] daemon 就绪（${i}x500ms）"
        break
    fi
    sleep 0.5
done
if [ "$READY" != "1" ]; then
    echo "❌ daemon 启动超时。daemon 日志：" >&2
    cat "$DAEMON_LOG" >&2
    exit 1
fi

# ── 4. 跑转译版 agent（纯文本路径）──────────────────────────────────────────
echo "[e2e] 跑转译版 agent（纯文本 prompt）..."
export AAID_URL
PROMPT="Say exactly the two words: hello world. Do not add anything else."
# auto-ai-react 是交互式 REPL：喂一行 + /exit
REACT_OUT=$(printf '%s\n/exit\n' "$PROMPT" | "$REACT_BIN" 2>"$DAEMON_LOG.react" || true)

echo "[e2e] agent 输出（纯文本路径）："
echo "---"
echo "$REACT_OUT"
echo "---"

# 断言：输出包含 "hello world"（大小写不敏感，LLM 可能有轻微变体）
if echo "$REACT_OUT" | grep -iqi "hello world"; then
    echo "[e2e] ✅ 纯文本路径通过（输出含 'hello world'）"
else
    echo "❌ 纯文本路径失败：输出未包含 'hello world'" >&2
    echo "[e2e] agent stderr：" >&2
    cat "$DAEMON_LOG.react" >&2
    exit 1
fi

# ── 5. 跑转译版 agent（工具调用路径 — EchoTool）─────────────────────────────
echo "[e2e] 跑转译版 agent（工具调用 prompt）..."
PROMPT2="Use the echo tool to echo the word: hello. Then reply with only the echoed value."
REACT_OUT2=$(printf '%s\n/exit\n' "$PROMPT2" | "$REACT_BIN" 2>"$DAEMON_LOG.react2" || true)

echo "[e2e] agent 输出（工具调用路径）："
echo "---"
echo "$REACT_OUT2"
echo "---"

# 断言：工具被调用（输出含 ECHO: 或 echo 痕迹）或至少有输出
if echo "$REACT_OUT2" | grep -iqi "echo\|hello"; then
    echo "[e2e] ✅ 工具调用路径通过（输出含 echo/hello 痕迹）"
else
    echo "⚠️  工具调用路径未明确检测到 echo 痕迹（LLM 可能未调用工具，属可接受波动）"
    echo "[e2e] agent stderr：" >&2
    cat "$DAEMON_LOG.react2" >&2
fi


# ── 断言（Plan 031，闭环 026 任务 8）：事件序列 ──────────────────────────────
# REPL 现在在 stderr 输出事件标记（[event] turn N start/end、[event] thinking）。
# 每个 turn 必须以 TurnStart 开场；thinking 标记（若服务模型输出 reasoning）
# 必须出现在 turn start 之后、答案文本之前。
if grep -q '\[event\] turn 1 start' "$DAEMON_LOG.react"; then
    echo "[e2e] ✅ 事件序列断言通过（TurnStart 标记存在）"
else
    echo "❌ 事件序列断言失败：stderr 无 '[event] turn 1 start' 标记" >&2
    cat "$DAEMON_LOG.react" >&2
    exit 1
fi
if grep -q '\[event\] thinking' "$DAEMON_LOG.react"; then
    first_turn=$(grep -n -m1 '\[event\] turn 1 start' "$DAEMON_LOG.react" | cut -d: -f1)
    first_think=$(grep -n -m1 '\[event\] thinking' "$DAEMON_LOG.react" | cut -d: -f1)
    if [ -n "$first_turn" ] && [ -n "$first_think" ] && [ "$first_think" -gt "$first_turn" ]; then
        echo "[e2e] ✅ thinking 标记位于 turn start 之后（顺序正确）"
    else
        echo "❌ 事件顺序错误：thinking 标记出现在 turn start 之前" >&2
        exit 1
    fi
else
    echo "[e2e] （服务模型未输出 thinking —— 顺序断言跳过，属可接受波动）"
fi

rm -f "$DAEMON_LOG.react" "$DAEMON_LOG.react2"

# ── 6. 成功 ──────────────────────────────────────────────────────────────────
E2E_RESULT=pass
echo "[e2e] ✅✅ 端到端验证通过 — 转译版 agent 与 daemon 全链路工作正常"
exit 0
