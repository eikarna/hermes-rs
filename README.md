![Kerux](assets/banner.png)

<p align="center">
  <a href="https://github.com/eikarna/hermes-rs/actions/workflows/ci.yml"><img src="https://github.com/eikarna/hermes-rs/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://kerux.eikarna.dev/"><img src="https://img.shields.io/badge/docs-kerux.eikarna.dev-00f0ff?logo=mdbook&logoColor=white" alt="Documentation"></a>
  <a href="https://github.com/eikarna/hermes-rs/releases"><img src="https://img.shields.io/github/v/release/eikarna/hermes-rs?label=release&color=e5a93b" alt="Release"></a>
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT-86847e" alt="License"></a>

</p>

<p align="center">
  <b>κῆρυξ — a fast, self-contained Rust agent for LLM-driven tool execution.</b><br>
  Streaming-first ReAct loop · ratatui TUI · Telegram/WhatsApp/Discord/Slack gateway · autonomous coding mode
</p>

---

![Kerux workspace](assets/chat.png)

## Highlights

- **Streaming-first ReAct loop** — tool calls detected and executed incrementally from partial LLM output, with a tolerant XML parser and self-healing re-prompts on failure
- **Ratatui TUI** — prompt-first landing screen, responsive workspace panes, reasoning rails, tool activity blocks, MCP/Skills/Behavior management, live telemetry HUD (tok/s, cache hits, context %, cost)
- **Messaging gateway** (`kerux serve`) — Telegram long-polling, WhatsApp bridge, Discord/Slack adapters, inbound HTTP webhooks for Slack and external automation, live status edits, streaming replies, tool approval, voice-note STT, cron scheduling, and subagent delegation
- **Autonomous coding mode** — 24/7 loop that reads `TODO.md`, validates with local tests, and only pushes after success
- **Persistent state** — sessions, memory, todos, and cron jobs survive restarts (atomic JSON writes under `~/.kerux/`)
- **Repo-map context** — tree-sitter symbols + personalized PageRank, token-budgeted into the system prompt
- **Taste profiles** — confidence-scored coding preferences injected from `.kerux/taste.json`, portable through `kerux taste push|pull`
- **Transactional git harness** — pre-run snapshots, Conventional Commits, TUI `/undo`
- **Post-edit validation gate** — successful edit tools run configured `[validation]` commands and return failures to the agent for self-repair; `kerux validate` runs the same policy manually
- **Flight recorder** — hash-chained run journals with read-only inspection and offline-verifiable proof capsules (see [docs/src/features/flight-recorder.md](docs/src/features/flight-recorder.md))
- **Fallback provider chain** — opt-in `[[client.fallback]]` failover across providers on 429/5xx/network errors, soak-tested and documented (see [docs/src/features/fallback-chain.md](docs/src/features/fallback-chain.md))
- **Cost guardrails** — `[budget]` spend ceilings enforced in the agent loop: one-time threshold warnings, pause/stop halts, one-time model downgrade, per-turn cost telemetry (see [docs/src/features/cost-guardrails.md](docs/src/features/cost-guardrails.md))

## Install

```bash
cargo install --path crates/kerux-cli
# or grab a prebuilt binary from GitHub Releases
```

## Quick start

```bash
kerux wizard                      # interactive setup (provider, key, model)
# or manually:
export OPENAI_API_KEY=***        # or: kerux auth login nous (OAuth, no key needed)

kerux chat                        # prompt-first TUI
kerux run --query "What is 2+2?"  # one-shot
kerux model                       # switch model anytime (fuzzy picker)
kerux serve                       # Messaging + webhook gateway
kerux autonomous                  # 24/7 coding loop
kerux validate                    # Run configured project validators once
kerux taste push team             # publish this project's learned style
kerux taste pull team             # merge that style into this project
```

First run with no config file and no provider credentials in the environment
auto-launches `kerux wizard` before `chat`/`run`/`serve`/`autonomous`.

## Documentation

Full docs live at **[kerux.eikarna.dev](https://kerux.eikarna.dev/)** — built with mdBook and refreshed automatically on every push:

| | |
|---|---|
| [Quickstart](https://kerux.eikarna.dev/quickstart.html) | Build, configure, first run |
| [Configuration](https://kerux.eikarna.dev/configuration.html) | TOML schema, env vars, auth profiles |
| [Architecture](https://kerux.eikarna.dev/architecture/overview.html) | Agent loop, streaming, tools |
| [Gateway & Adapters](https://kerux.eikarna.dev/architecture/gateway.html) | Telegram, WhatsApp, Discord & Slack |
| [Features](https://kerux.eikarna.dev/features/tool-approval.html) | Approval, compaction, fallbacks, STT, cron, delegation |
| [Roadmap](https://kerux.eikarna.dev/development/roadmap.html) | What's next |

Screenshots in this README and the docs are **generated automatically** by CI (`kerux screenshot` renders the TUI headlessly) whenever the UI changes.

## Development

```bash
cargo build --workspace
cargo test --workspace     # 534 tests (424 core + 110 cli)
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for conventions and the PR process.

## License

Licensed under the [MIT License](LICENSE).


