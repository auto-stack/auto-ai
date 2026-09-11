# AutoOS AI Infrastructure

Shared LLM harness for all AutoOS applications — concurrency arbitration, API key vault, model routing, and usage tracking.

## Crates

| Crate | Binary | Purpose |
|-------|--------|---------|
| `auto-ai-client` | — | Shared client library. All apps link this to call LLM services. |
| `auto-ai-daemon` | `aaid` | System daemon. HTTP server with global concurrency pools, key vault, model routing. |
| `aictl` | `aictl` | CLI management tool for the daemon. |

## Architecture

```
┌──────────────────────────────────────────────────────┐
│           aaid (daemon)                              │
│                                                      │
│  HTTP over Unix socket / TCP localhost               │
│  POST /v1/chat/completions  → 并发仲裁 → 上游 LLM API │
│  GET  /v1/status            → 并发池状态              │
│  GET  /v1/models            → 可用模型                │
│  GET  /v1/usage             → token 用量             │
│                                                      │
│  Semaphore per provider  |  Key Vault  |  Usage Tracker │
└───────┬───────────────────┬──────────────────────────┘
        │                   │
   ┌────┴────┐         ┌────┴────┐
   │  Ash    │         │ Forge   │   ... all AutoOS apps
   │ auto-ai-client    │ auto-ai-client
   └─────────┘         └─────────┘
```

## Quick Start

```bash
# Start the daemon (auto-detects API keys from env)
ZHIPU_API_KEY=your-key aaid

# Check status
aictl status

# Apps link auto-ai-client and call AiClient::complete()
```

## Command execution (ash-first)

`auto-ai-cli`'s `run_command` / `run_ash_script` tools execute commands through
the sibling [auto-shell](../auto-shell) `ash` binary when it is available:
`ash --sandbox <cwd>` confines file operations to the working directory, and
the tool falls back to the system shell (`cmd.exe` / `sh`) only when ash is
absent or could not even start a command. Policy denials and genuine command
failures never fall back — they surface to the model as PAUSED notices or real
output instead. Discovery order: `AUTO_AI_ASH_BIN` (authoritative override) →
`PATH` → sibling `auto-shell/ash/target/{release,debug}` heuristic; set
`AUTO_AI_ASH_AUDIT=<file>` to turn on ash's JSONL audit log. Contract details:
[`docs/specs/auto-ai-cli/shell-execution.md`](docs/specs/auto-ai-cli/shell-execution.md)
(PLAN-033, design in `docs/designs/2026-09-11-ash-first-shell-execution-design.md`).

## Design Doc

See [`docs/design/15-ai-daemon-infrastructure.md`](https://github.com/auto-stack/auto-lang/blob/master/docs/design/15-ai-daemon-infrastructure.md) in the auto-lang repo.

## License

MIT
