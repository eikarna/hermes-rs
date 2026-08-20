# Configuration

Kerux is TOML-first. Config resolution order:

1. `--config <path>` CLI flag
2. `KERUX_HOME` environment variable
3. `~/.config/kerux/kerux.toml` (Unix) / `%APPDATA%\kerux\kerux.toml` (Windows)
4. `./kerux.toml` in the current directory

See [`kerux.example.toml`](https://github.com/eikarna/hermes-rs/blob/main/kerux.example.toml) for the full annotated reference.

## Key Sections

### `[client]`

LLM provider settings: `provider`, `model`, `base_url`, `timeout_secs`, `stream`.

### `[gateway]`

Messaging gateway settings:

| Key | Default | Description |
|---|---|---|
| `telegram_enabled` | `false` | Enable Telegram long-polling adapter |
| `telegram_token` | — | Bot token |
| `whatsapp_enabled` | `false` | Enable WhatsApp adapter (Baileys bridge) |
| `whatsapp_bridge_url` | `http://127.0.0.1:3000` | Bridge endpoint |
| `streaming_replies` | `false` | Live-edit token streaming with `▌` cursor |
| `tool_approval` | `false` | Require Telegram inline-keyboard approval before tool execution |
| `context_compaction` | `false` | Summarize oldest messages near context cap |
| `stt_model` | — | Voice note transcription model (enables STT) |

### `[delegation]`

Subagent delegation settings: `provider`, `model`, `max_concurrent` (default 3).

### `[fallback]`

Fallback provider chain (default OFF): ordered list of providers tried on transient failure.

### `[autonomous]`

Autonomous coding mode: `todo_path` (default `TODO.md`), `status_path` (default `autonomous-status.toml`), test command, git remote/branch.

## Environment Variables

All config fields can be overridden via `KERUX_*` env vars (e.g. `KERUX_PROVIDER`, `KERUX_MODEL`, `KERUX_HOME`, `KERUX_LOG_LEVEL`, `KERUX_SKILLS_DIR`).
