# Kerux

**Kerux** (Greek: κῆρυξ — *herald, messenger of the gods*) is a fast, self-contained AI agent runtime written in Rust.

It runs a full ReAct agent loop with tool execution, streaming responses, persistent memory, and multi-platform messaging gateways (Telegram, WhatsApp) — all in a single static binary with zero runtime dependencies.

## Why Kerux?

- **Single binary** — no Python, no Node, no runtime. `cargo build` and run.
- **Fast** — Rust core, ~30K LOC, sub-second startup.
- **Self-contained** — sessions, memory, todos, cron jobs all persist to disk as JSON.
- **Multi-platform** — Telegram long-polling + WhatsApp (Baileys bridge) adapters built in.
- **Production features** — tool approval gates, context compaction, fallback provider chains, voice STT, cron scheduling, subagent delegation.

## Crate Layout

| Crate | Description |
|---|---|
| `kerux-core` | Agent loop, LLM clients, tools, gateway adapters, persistence |
| `kerux-cli` | CLI/TUI frontend, `serve` gateway mode, autonomous coding mode |

## Attribution

Kerux began as a Rust port of [hermes-agent](https://github.com/NousResearch/hermes-agent) by Nous Research. See [ATTRIBUTION.md](https://github.com/eikarna/hermes-rs/blob/main/ATTRIBUTION.md) for details.
