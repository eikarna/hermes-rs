# Development History

**Last Updated:** 2026-08-20
**Current Version:** v0.2.0 (main branch)
**Status:** ✅ Stable — all CI workflows green

---

## Timeline

### v0.2.0 — Gateway Era (2026-08-18 → 2026-08-20)

The project grew from a CLI/TUI agent into a full messaging-gateway runtime, then shed its upstream-derived name.

**Gateway & messaging**
- Wired the gateway into the CLI (`kerux serve`) with Telegram long-polling — no webhook server needed
- Markdown → Telegram MarkdownV2 converter (stdlib only): special-char escaping, code fences, blockquotes, tables → bullet lists, `---` → unicode separator, auto-fallback to plain text
- Message chunking at 3500 chars with code-fence tracking (no more `message is too long`)
- Event-driven UX: live status edits (`🤔 Thinking…`, `🔧 Tool`, heartbeats), final reply replaces the status message
- Dual reply modes: `streaming_replies = false` (default) or `true` (live token streaming with `▌` cursor, ~900ms throttle)
- WhatsApp adapter via Baileys HTTP bridge (`/messages` drain-queue polling, WhatsApp-specific markdown converter)
- Parallel per-adapter polling tasks (Telegram long-poll no longer starves WhatsApp)

**Persistence**
- `SessionStore`: per-channel conversation history as atomic JSON files (`~/.kerux/sessions/`), format v2 with persistent `summary` + v1 backward-compat
- Shared `persist.rs` helper (atomic write via tempfile + rename, `KERUX_HOME` override)
- File-backed persistence for memory (`memories.json`) and todos (`todos.json`)

**Reliability sweep (zero-bug audit)**
- Cooperative cancellation (`Arc<AtomicBool>`) checked across the agent loop, SSE chunks, and tool execution; conversation repair after cancel
- Non-blocking event emission (`try_send`) — no more deadlocks when the event channel fills
- Explicit HTTP timeouts on Telegram/WhatsApp clients (connect 5s, read 30–45s)
- Identity-checked `active_runs` deregistration (interrupt race fix)
- Char-safe string truncation (no UTF-8 panics on emoji/CJK tool args)
- Headless logging fix (`IsTerminal` check — logs no longer vanish in `serve` mode)

**Feature set**
- Tool approval via Telegram inline keyboards (`[✅ Approve] [❌ Deny]`, callback-query routing, per-call oneshot channels)
- Rolling context compaction: one-shot LLM summarization of oldest turns near the session cap, `[CONTEXT SUMMARY]` marker
- Fallback provider chain (`FallbackChainProvider`, transient-error detection) — toggleable, default OFF
- Voice-note STT: Telegram audio → `POST /v1/audio/transcriptions`
- Cron scheduler: interval jobs with atomic JSON persistence, `/cron add|list|pause|resume|remove`
- Subagent delegation tool: isolated child agents, max 3 concurrent (semaphore), depth 1

**Rebrand & docs**
- Full rebrand `hermes-rs` → **kerux** (crates, binary, env vars, data dirs, context files) — commit `54076ee`
- mdBook documentation site at [kerux.eikarna.dev](https://kerux.eikarna.dev/), deployed via GitHub Actions
- Root cleanup: design docs moved into `docs/`, stale files removed

### v0.1.x — Aider Integration (2026-08-07 → 2026-08-17)

See [roadmap.md](roadmap.md) for the original plan. All five phases shipped:

| Phase | Feature | Commits |
|-------|---------|---------|
| 1 | Model-agnostic provider routing + capability tables (OpenAI, Anthropic, Ollama, OpenRouter, Gemini) | `aa53ed6` |
| 2 | Tree-sitter AST repo map + PageRank scoring (C/Python/Rust/TypeScript) | `b141e7d`, `bf25a0a` |
| 3 | Aider-style SEARCH/REPLACE edit blocks with exact+fuzzy matching | `a697180` |
| 4 | Transactional git harness: snapshots, dirty-tree guard, Conventional Commits, `/undo` | `2e7a75a` |
| 5 | Skill & memory lifecycle: curator passes, decay/prune/dedup, distillation, pinning | `aa53ed6`, `a309db0`, `2a4485a` |

Also merged from contributors: Nous Portal OAuth for core (#49) and CLI (#50).

---

## Test & CI Status

- **kerux-core:** 318 tests passing
- **kerux-cli:** 105 tests passing
- **CI:** fmt, clippy (`-D warnings`), rustdoc (`-D warnings`), build, test, release, docs — all green on GitHub Actions

## Known Issues & Debt

- **Huge repos (>6k files):** discovery scans the full tree before capping; early-return optimization pending
- **Tree-sitter on >1MB files:** parser may stall; size-threshold warning not yet implemented
- **Vision input:** `supports_vision` capability field exists but no multimodal pipeline yet
