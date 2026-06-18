# AutoOS AI — three-crate architecture

This document describes the responsibilities of the four crates in this repo
after the **Plan 002 three-crate refactor** and how they cooperate.

## Crate roles

```
┌──────────────────────────────────────────────────────────────────────┐
│  ai-config  (shared)                                                  │
│  • canonical wire types (Message/ContentBlock/CompletionRequest/…)    │
│  • unified ProviderConfig + .at parsing (auto-atom)                   │
│  • model-existence validation                                         │
└──────┬───────────────────────────────────────────────────────────────┘
       │ depended on by all three below
       ├────────────────────┬─────────────────────────┐
       ▼                    ▼                         ▼
┌──────────────┐    ┌───────────────┐         ┌──────────────┐
│ auto-ai-     │    │ auto-ai-      │         │ auto-ai-agent│
│ daemon (aaid)│    │ client (thin) │         │              │
│              │    │               │         │ Profession / │
│ • ALL LLM    │    │ • sends       │         │ Agent /      │
│   API comms  │◄───│   canonical   │────────►│ Workflow     │
│ • canonical↔ │HTTP│   requests    │ (uses   │ • validates  │
│   provider   │    │ • receives    │  client)│   Profession │
│   conversion │    │   canonical   │         │   models via │
│ • concurrency│    │   responses   │         │   ai-config  │
│ • usage      │    │ • no provider │         │              │
│ • (provider/ │    │   knowledge   │         │              │
│   format/sse)│    │ • no direct   │         │              │
│              │    │   LLM mode    │         │              │
└──────┬───────┘    └───────────────┘         └──────────────┘
       │
       ▼  aictl only observes the daemon (status/models/usage)
   ┌────────┐
   │ aictl  │
   └────────┘
```

### `ai-config`
The shared foundation. Defines the **canonical wire format** (the neutral
`ContentBlock` model that flows between client and daemon) and parses the two
`.at` config files. Both client and daemon depend on it, so neither has a
private copy of the provider layout or the wire types.

### `auto-ai-daemon` (binary `aaid`)
The **single LLM gateway**. It owns all provider knowledge: building upstream
requests (OpenAI / Anthropic wire format), parsing responses, and translating
to/from the canonical format. It also owns the concurrency pools, the API-key
vault, and per-app usage tracking. Apps never talk to an LLM directly — every
request goes through the daemon.

### `auto-ai-client`
A **thin HTTP client** for the daemon. It sends canonical
`CompletionRequest`s and parses canonical `CompletionResponse`s. It carries no
provider knowledge and has no "direct" LLM mode — the daemon is a mandatory
dependency. Its only other job is auto-discovering (and lazy-starting) the
daemon (`ensure_daemon`, ssh-agent model).

### `auto-ai-agent`
The agent layer (Profession library, ReAct loop, Workflow engine). It builds
on `auto-ai-client` and validates that a Profession's `model()` is actually
served by the configured providers (via `ai-config`), failing fast with a
clear message instead of a run-time daemon 404.

### `aictl`
The daemon control CLI. It talks only to the daemon (`/v1/status`,
`/v1/models`, `/v1/usage`) and observes concurrency/token state. It does **not**
see agent/workflow-level task state — that's by design (the daemon stays free
of business logic).

## Data flow (a completion request)

```
agent.run()
  → auto_ai_client::AiClient::complete(canonical CompletionRequest)
      → HTTP POST aaid /v1/chat/completions  (canonical body)
          daemon: acquire concurrency permit
                → ProviderRegistry picks provider
                → provider translates canonical → wire (OpenAI/Anthropic)
                → calls upstream LLM
                → parses response → canonical CompletionResponse
                → records usage
                ← canonical response
      ← canonical CompletionResponse
  ← content / tool_calls to the agent
```

## Configuration

Two `.at` files, both parsed by `ai-config` using the shared `auto-atom`
parser. Each is a **single-root** document (`client { … }` / `daemon { … }`)
because auto-atom parses exactly one root value.

- `~/.config/autoos/ai-client.at` — providers + defaults (client view).
- `~/.config/autoos/ai-daemon.at` — same providers + daemon-only fields
  (`listen_addr`, `idle_timeout_min`, `log_level`, per-provider
  `max_concurrency`) and real API keys.

Model names are quoted (`["glm-4.5", "glm-flash"]`) because they contain dots
that auto-atom would otherwise read as a number literal. See
`crates/ai-config/examples/` for full examples.

**Env-var fallback:** if no config file is present, both client and daemon
fall back to `ZHIPU_API_KEY` / `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`
environment variables.

## Migrating an existing config (Plan 002)

If you have a legacy flat `ai-client.at` / `ai-daemon.at` (pre-Plan-002), wrap
its contents in a `client { … }` / `daemon { … }` root and quote your model
names. See the example files for the exact shape.
