# Gateway & Adapters

The gateway (`kerux-core/src/gateway.rs`) connects the agent to messaging platforms. Started via `kerux serve`.

## Design

- **Mixed transports** — Telegram uses long-polling while Slack and external automation enter through an optional axum HTTP listener.
- **Parallel adapters** — each adapter runs in its own `tokio::spawn` task so one platform's polling never starves another's.
- **Interrupt-on-new-message** — an incoming message cancels the active run for that channel and starts a fresh one.

## Adapters

| Adapter | Transport | Enable |
|---|---|---|
| Telegram | Long-polling (`getUpdates`) | `[gateway] telegram_enabled` + `telegram_token` |
| WhatsApp | Baileys HTTP bridge (drain-queue polling) | `[gateway] whatsapp_enabled` + `whatsapp_bridge_url` |
| Discord | REST API (`discord_api_base`, default `https://discord.com/api/v10`): token verification via `/users/@me`, send via `/channels/{id}/messages`, message-create event parsing | `[gateway] discord_enabled` + `discord_token` |
| Slack | Events API inbound webhook + REST replies | `[gateway] slack_enabled` + `slack_token` + `slack_signing_secret` + webhooks |

Discord remains a minimal REST integration without a websocket gateway connection. Slack Events API requests are accepted at `/webhook/slack`, verified using Slack's HMAC-SHA256 signature and five-minute replay window, then routed through the Slack adapter for replies.

## Inbound HTTP webhooks

Set `webhooks_enabled = true` and `webhooks_addr = "127.0.0.1:8080"` under `[gateway]`, then run `kerux serve`.

| Endpoint | Method | Purpose |
|---|---|---|
| `/health` | `GET` | Listener readiness check |
| `/webhook` or `/webhook/generic` | `POST` | Generic external trigger |
| `/webhook/slack` | `POST` | Slack Events API challenge and event callbacks |

Generic triggers accept JSON:

```json
{
  "message": "Inspect the latest CI failure",
  "source": "ci",
  "target": "telegram:12345",
  "metadata": { "build": 42 }
}
```

`message` is required. `source` becomes the synthetic sender ID. `target` is optional; when present it must use `platform:channel` format and routes progress/final output through that registered platform adapter. Without `target`, the run is fire-and-forget. The listener returns `202 Accepted` after queueing a valid trigger.

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
| Cron jobs | `~/.kerux/scheduler.json` |

All writes are atomic (temp file + rename). Set `KERUX_HOME` to relocate the state root.
