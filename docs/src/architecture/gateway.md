# Gateway & Adapters

The gateway (`kerux-core/src/gateway.rs`) connects the agent to messaging platforms. Started via `kerux serve`.

## Design

- **Long-polling first** — Telegram uses `getUpdates` with 30s timeout; no HTTP server dependency.
- **Parallel adapters** — each adapter runs in its own `tokio::spawn` task so one platform's polling never starves another's.
- **Interrupt-on-new-message** — an incoming message cancels the active run for that channel and starts a fresh one.

## Adapters

| Adapter | Transport | Enable |
|---|---|---|
| Telegram | Long-polling (`getUpdates`) | `[gateway] telegram_enabled` + `telegram_token` |
| WhatsApp | Baileys HTTP bridge (drain-queue polling) | `[gateway] whatsapp_enabled` + `whatsapp_bridge_url` |
| Discord | REST API (`discord_api_base`, default `https://discord.com/api/v10`): token verification via `/users/@me`, send via `/channels/{id}/messages`, message-create event parsing | `[gateway] discord_enabled` + `discord_token` |
| Slack | REST API (`slack_api_base`, default `https://slack.com/api`): token verification, `send_message`, update-event parsing | `[gateway] slack_enabled` + `slack_token` |

Discord and Slack adapters are minimal REST integrations (no websocket gateway connection yet); incoming events are parsed from posted updates.

## Shared UX

- Markdown conversion per platform: MarkdownV2 for Telegram (stdlib converter: special-char escaping, fenced code blocks, `---` → Unicode separator, tables → bullet lists, blockquotes), a dedicated WhatsApp converter (`markdown_to_whatsapp`)
- Message chunking at 3500 chars on line boundaries with code-fence tracking
- Live message editing for status updates (`🤔 Thinking...`, `🔧 Tool`, heartbeats)
- Two reply modes: **normal** (final edit) or **streaming** (`streaming_replies = true`, token streaming with `▌` cursor, ~900ms throttle)
- Inline-keyboard tool approval (Telegram) via `callback_query`
- Voice note STT via `getFile` → download → `/v1/audio/transcriptions`
- Explicit HTTP timeouts everywhere (connect 5s; read 30–45s) — no half-open hangs

## Persistence

All channel state survives restarts:

| Data | Location |
|---|---|
| Conversations | `~/.kerux/sessions/<platform>_<channel>.json` (format v2) |
| Memory | `~/.kerux/memory/memories.json` |
| Todos | `~/.kerux/todos/todos.json` |
| Cron jobs | `~/.kerux/cron/jobs.json` |

All writes are atomic (temp file + rename). Set `KERUX_HOME` to relocate the state root.
