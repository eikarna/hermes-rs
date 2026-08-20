# Roadmap

Where Kerux is heading. For what already shipped, see [history.md](history.md).

---

## Now (v0.2.x)

Stabilization of the gateway era:

- **Harden `kerux serve`** — soak testing, reconnect/backoff edge cases, multi-channel load
- **Docs polish** — flesh out feature pages with real config examples and screenshots
- **CI screenshot previews** — automated TUI/docs screenshots on every release

## Next (v0.3)

- **Vision input** — `supports_vision` capability is already plumbed; needs the multimodal pipeline (image preprocessing, per-provider request construction). Anthropic + Ollama first
- **Repo map optimizations** — early-return discovery once the 500-file cap is hit; size-threshold guard for tree-sitter on >1MB files
- **Webhook transport** — optional HTTP listener alongside long-polling (for platforms without polling APIs)
- **Session branching** — fork a conversation from any point in history

## Later (ideas, unscheduled)

- Plugin system for custom tools beyond MCP
- Multi-agent routing (different models per channel/task type)
- Local embedding-based memory search (currently keyword)
- Voice output (TTS replies)

## Done (summary)

- ✅ Aider integration phases 1–5 (providers, repo map, edit blocks, git harness, curator) — see [history.md](history.md)
- ✅ Gateway: Telegram long-polling + WhatsApp bridge, MarkdownV2 conversion, streaming replies
- ✅ Persistence: sessions (v2 + compaction summaries), memory, todos — all atomic JSON
- ✅ Tool approval, fallback provider chain, voice-note STT, cron scheduler, subagent delegation
- ✅ Rebrand to **kerux** + mdBook docs at [kerux.eikarna.dev](https://kerux.eikarna.dev/)
