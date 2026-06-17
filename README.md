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

## Design Doc

See [`docs/design/15-ai-daemon-infrastructure.md`](https://github.com/auto-stack/auto-lang/blob/master/docs/design/15-ai-daemon-infrastructure.md) in the auto-lang repo.

## License

MIT
