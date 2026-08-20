# Architecture Overview

```
┌─────────────────────────────────────────────────────┐
│                    kerux-cli                         │
│  TUI (ratatui) │ serve (gateway) │ autonomous mode  │
└────────────────────────┬────────────────────────────┘
                         │
┌────────────────────────▼────────────────────────────┐
│                   kerux-core                         │
│                                                      │
│  ┌──────────┐  ┌──────────┐  ┌───────────────────┐  │
│  │  Agent   │  │  Client  │  │      Gateway      │  │
│  │  (ReAct) │  │  (LLM)   │  │ Telegram│WhatsApp │  │
│  └────┬─────┘  └──────────┘  └───────────────────┘  │
│       │                                              │
│  ┌────▼─────────────────────────────────────────┐   │
│  │  Tools: file, patch, terminal, code_exec,    │   │
│  │  web, memory, todo, sub_agent, mcp, skills   │   │
│  └──────────────────────────────────────────────┘   │
│                                                      │
│  ┌──────────────────────────────────────────────┐   │
│  │  Persistence: sessions, memory, todos, cron  │   │
│  │  (~/.kerux/ — atomic JSON writes)            │   │
│  └──────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────┘
```

## Core Subsystems

| Module | Responsibility |
|---|---|
| `agent.rs` | ReAct loop, streaming, cooperative cancellation, approval gate, context compaction |
| `client.rs` | LLM provider abstraction (OpenAI-compatible, Anthropic, Gemini), fallback chain |
| `gateway.rs` | Platform adapters (Telegram long-polling, WhatsApp bridge), markdown conversion, message chunking |
| `session_store.rs` | Per-channel conversation persistence (format v2 with summary) |
| `persist.rs` | Shared atomic JSON write helpers (`~/.kerux/`) |
| `approval.rs` | Tool approval gate (inline keyboard via Telegram callback_query) |
| `scheduler.rs` | Cron-style job scheduler with disk persistence |
| `tools/` | Built-in tool implementations |
| `platform.rs` | OS paths (`kerux_home()`, config/data/sessions dirs) |
