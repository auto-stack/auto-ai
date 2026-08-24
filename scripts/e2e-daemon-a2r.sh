#!/usr/bin/env bash
# e2e-daemon-a2r.sh — Plan 025 Phase 5 — 转译版 daemon 端到端验证 runner
#
# 全 Auto 版链路：转译版 agent (auto-ai-react) → 转译版 client (a2r) →
# 转译版 daemon (aaid-a2r) → LLM。验证 daemon 这一层从 .at 转译后能真正工作。
#
# 与 scripts/e2e-transpiled.sh 的唯一区别：daemon 用转译版 (aaid-a2r) 而非
# 原生版 (aaid)。agent + client 两层在 Plan 022/024 已验证过，本脚本聚焦
# daemon 转译层的端到端正确性。
#
# 用法：bash scripts/e2e-daemon-a2r.sh
#   前置：ZHIPU_API_KEY（或 OPENAI_API_KEY / ANTHROPIC_API_KEY）或配置文件
#   退出码 0 = 端到端验证通过
#   退出码 1 = 失败
#
# 可选环境变量：
#   AAID_URL      — daemon 地址（默认 http://127.0.0.1:17654）
#   SKIP_BUILD    — 设为 1 跳过 cargo build
#
# 详见 D:/autostack/auto-ai/docs/plans/archive/025-daemon-autoization.md Phase 5

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
AAID_URL="${AAID_URL:-http://127.0.0.1:17654}"
DAEMON_PORT="${DAEMON_PORT:-17654}"
DAEMON_PID=""
DAEMON_LOG="$(mktemp -t aaid-a2r-e2e-XXXXXX 2>/dev/null || mktemp "${TMPDIR:-/tmp}/aaid-a2r-e2e.XXXXXX.log")"

# ── 清理：脚本退出时 kill daemon ────────────────────────────────────────────
cleanup() {
    if [ -n "$DAEMON_PID" ]; then
        echo "[e2e] stopping transpiled daemon (pid $DAEMON_PID)..."
        kill "$DAEMON_PID" 2>/dev/null || true
        sleep 1
        kill -9 "$DAEMON_PID" 2>/dev/null || true
        # Windows: ensure the process tree is gone (Git Bash kill may not reap children)
        taskkill //F //IM aaid-a2r.exe 2>/dev/null || true
    fi
    if [ "${E2E_RESULT:-fail}" = "pass" ]; then
        rm -f "$DAEMON_LOG"
    fi
}
trap cleanup EXIT

echo "[e2e] === Plan 025 Phase 5 转译版 daemon 端到端验证 ==="

# ── 1. 前置检查：API key ────────────────────────────────────────────────────
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
    echo "❌ 缺少 API key（设 ZHIPU_API_KEY/OPENAI_API_KEY/ANTHROPIC_API_KEY，或在配置文件内联）" >&2
    exit 1
fi
echo "[e2e] API key 检测通过（env=$has_env_key, config_file=$has_cfg_key）"

# ── 2. 构建二进制 ────────────────────────────────────────────────────────────
if [ "${SKIP_BUILD:-0}" != "1" ]; then
    echo "[e2e] 构建转译版 daemon (aaid-a2r)..."
    (cd "$REPO_ROOT/crates/auto-ai-daemon/rust" && cargo build --bin aaid-a2r) >/dev/null 2>&1
    echo "[e2e] 构建转译版 agent (auto-ai-react)..."
    (cd "$REPO_ROOT/crates/auto-ai-agent/rust" && cargo build --bin auto-ai-react) >/dev/null 2>&1
fi

AAID_BIN="$REPO_ROOT/crates/auto-ai-daemon/rust/target/debug/aaid-a2r"
REACT_BIN="$REPO_ROOT/crates/auto-ai-agent/rust/target/debug/auto-ai-react"

if [ ! -f "$AAID_BIN" ] || [ ! -f "$REACT_BIN" ]; then
    echo "❌ 二进制不存在：$AAID_BIN 或 $REACT_BIN（构建失败？）" >&2
    exit 1
fi

# ── 3. 拉起转译版 daemon ────────────────────────────────────────────────────
echo "[e2e] 启动转译版 daemon ($AAID_URL)..."
# Clear any lingering instance holding the port (Windows file/socket lock).
taskkill //F //IM aaid-a2r.exe 2>/dev/null || true
sleep 0.5
"$AAID_BIN" --listen "127.0.0.1:$DAEMON_PORT" >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!

# 等待 daemon 就绪（轮询 /v1/status，最多 ~10s）
echo "[e2e] 等待转译版 daemon 就绪..."
READY=0
for i in $(seq 1 20); do
    if curl -sf "$AAID_URL/v1/status" >/dev/null 2>&1; then
        READY=1
        echo "[e2e] 转译版 daemon 就绪（${i}x500ms）"
        break
    fi
    sleep 0.5
done
if [ "$READY" != "1" ]; then
    echo "❌ 转译版 daemon 启动超时。daemon 日志：" >&2
    cat "$DAEMON_LOG" >&2
    exit 1
fi

# ── 4. /v1/status 结构断言（转译版 server.at 的 status handler）─────────────
echo "[e2e] 校验 /v1/status 响应结构..."
STATUS_JSON=$(curl -s "$AAID_URL/v1/status")
if echo "$STATUS_JSON" | grep -q '"status":"running"' \
   && echo "$STATUS_JSON" | grep -q '"pools"'; then
    echo "[e2e] ✅ /v1/status 结构正确（含 status + pools）"
else
    echo "❌ /v1/status 结构异常：$STATUS_JSON" >&2
    exit 1
fi

# ── 5. 跑转译版 agent（纯文本路径）──────────────────────────────────────────
# 全链路：转译版 agent → 转译版 client → 转译版 daemon → LLM
echo "[e2e] 跑转译版 agent → 转译版 daemon（纯文本 prompt）..."
export AAID_URL
PROMPT="Say exactly the two words: hello world. Do not add anything else."
REACT_OUT=$(printf '%s\n/exit\n' "$PROMPT" | "$REACT_BIN" 2>"$DAEMON_LOG.react" || true)

echo "[e2e] agent 输出（纯文本路径）："
echo "---"
echo "$REACT_OUT"
echo "---"

if echo "$REACT_OUT" | grep -iqi "hello world"; then
    echo "[e2e] ✅ 纯文本路径通过（输出含 'hello world'）"
else
    echo "❌ 纯文本路径失败：输出未包含 'hello world'" >&2
    echo "[e2e] agent stderr：" >&2
    cat "$DAEMON_LOG.react" >&2
    exit 1
fi


# ── 断言（Plan 031，闭环 026 任务 8）：事件序列 ──────────────────────────────
# REPL 在 stderr 输出事件标记（[event] turn N start/end、[event] thinking）。
# 每个 turn 必须以 TurnStart 开场；thinking 标记（若服务模型输出 reasoning）
# 必须出现在 turn start 之后。
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

rm -f "$DAEMON_LOG.react"

# ── 6. 成功 ──────────────────────────────────────────────────────────────────
E2E_RESULT=pass
echo "[e2e] ✅✅ 转译版 daemon 端到端验证通过"
echo "[e2e]     全 Auto 链路：转译版 agent → 转译版 client → 转译版 daemon → LLM"
exit 0
