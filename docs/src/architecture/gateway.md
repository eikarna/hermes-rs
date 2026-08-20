# Gateway & Adapters

The gateway (`kerux-core/src/gateway.rs`) connects the agent to messaging platforms. Started via `kerux serve`.

## Design

- **Long-polling, not webhooks** — no HTTP server dependency (YAGNI). Telegram uses `getUpdates` with 30s timeout.
- **Parallel adapters** — each adapter runs in its own `tokio::spawn` task so Telegram's long-poll never starves WhatsApp polling.
- **Interrupt-on-new-message** — an incoming message cancels the active run for that channel and starts a fresh one.

## Telegram Adapter

- Markdown → MarkdownV2 conversion (stdlib, custom converter): special-char escaping, fenced code blocks, `---` → Unicode separator, tables → bullet lists, blockquotes
- Message chunking at 3500 chars on line boundaries with code-fence tracking
- Live message editing for status updates (`🤔 Thinking...`, `🔧 Tool`, heartbeats)
- Two reply modes: **normal** (final edit) and **streaming** (`streaming_replies = true`, token streaming with `▌` cursor, ~900ms throttle)
- Inline-keyboard tool approval via `callback_query`
- Voice note STT via `getFile` → download → `/v1/audio/transcriptions`
- Explicit HTTP timeouts (connect 5s, read 45s) — no half-open hangs

## WhatsApp Adapter

- Talks to a Baileys HTTP bridge (default `http://127.0.0.1:3000`)
- Polls `GET /messages` (drain-queue), sends via `POST /send`
- Custom markdown converter (`markdown_to_whatsapp`) — no regex lookarounds
- Explicit HTTP timeouts (connect 5s, read 30s)

## Persistence

All channel state survives restarts:

| Data | Location |
|---|---|
| Conversations | `~/.kerux/sessions/<platform>_<channel>.json` (format v2) |
| Memory | `~/.kerux/memory/memories.json` |
| Todos | `~/.kerux/todos/todos.json` |
| Cron jobs | `~/.kerux/cron/jobs.json` |

All writes are atomic (temp file + rename).
