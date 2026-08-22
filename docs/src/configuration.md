# Configuration

Kerux is TOML-first. Config resolution order:

1. `--config <path>` CLI flag
2. `./kerux.toml` in the current directory
3. `./.kerux.toml` in the current directory
4. `~/.config/kerux/config.toml` (Unix) / `%APPDATA%\kerux\config.toml` (Windows)

If none exist, built-in defaults are used. State (sessions, memory, todos, cron jobs, run journals) lives under `~/.kerux/`, relocatable with `KERUX_HOME`.

See [`kerux.example.toml`](https://github.com/eikarna/hermes-rs/blob/main/kerux.example.toml) for the full annotated reference.

## Key Sections

### `[client]`

LLM provider settings: `provider`, `base_url`, `api_key`, `auth_ref`, per-provider endpoint overrides (`[client.openai]`, `[client.anthropic]`, `[client.ollama]`, `[client.openrouter]`, `[client.gemini]`), and `timeout_secs`. The model itself is set under `[agent] model`.

### `[agent]`

Agent behavior: `model` (default `gpt-4`), `max_iterations` (20), `context_window` (128000), `stream` (true), `repo_map_tokens` (0 = off), `repo_map_max_files` (500), `edit_format_override`, `max_repair_attempts`, `auto_commit` (false).

### `[gateway]`

Messaging gateway settings:

| Key | Default | Description |
|---|---|---|
| `telegram_enabled` | `false` | Enable Telegram long-polling adapter |
| `telegram_token` | — | Bot token |
| `discord_enabled` | `false` | Enable Discord REST adapter (`discord_token`) |
| `slack_enabled` | `false` | Enable Slack REST adapter (`slack_token`) |
| `whatsapp_enabled` | `false` | Enable WhatsApp adapter (Baileys bridge) |
| `whatsapp_bridge_url` | — | Bridge endpoint (e.g. `http://127.0.0.1:3000`) |
| `streaming_replies` | `false` | Live-edit token streaming with `▌` cursor |
| `tool_approval` | `true` | Require inline-keyboard approval before dangerous tool execution |
| `tool_approval_timeout_secs` | `300` | Auto-deny approval requests after this long |
| `context_compaction` | `true` | Summarize oldest messages near context cap |
| `stt_model` | — | Voice note transcription model (enables STT) |

### `[[client.fallback]]`

Fallback provider chain (default OFF): array-of-tables entries (`provider`, optional `base_url`, `api_key`, `model`, `timeout_secs`) tried in order when the primary provider hits transient failures (network errors, 429, 5xx). Auth failures and bad requests propagate immediately.

### `[validation]`

Deterministic project validators (default disabled): `enabled`, `fail_fast`, plus `[[validation.validators]]` entries (`name`, `command`, `required`, `timeout_secs`). Executed by the validation engine with outcomes journaled as evidence.

### `[recorder]`

Flight recorder policy: `enabled`, `record_content`, `record_reasoning`, `max_payload_bytes`, `failure_mode` (`warn` | `fail`). Journals every agent run to a hash-chained store under `~/.kerux/runs/`.

### `[autonomous]`

Autonomous coding mode: `todo_path` (default `TODO.md`), `status_path` (default `autonomous-status.toml`), `test_command` (default `cargo test --workspace`), `interval_secs` (300), `git_remote` (origin), `git_branch` (agent-dev), `command_timeout_secs` (900), `max_failures_per_state` (3).

## Environment Variables

Selected fields have env overrides applied after the TOML is parsed:

| Variable | Overrides |
|---|---|
| `KERUX_PROVIDER` / `OPENAI_BASE_URL` | `[client] provider` / `base_url` |
| `OPENAI_API_KEY` | `[client] api_key` |
| `KERUX_AUTH_REF` | `[client] auth_ref` |
| `KERUX_MODEL` / `KERUX_STREAM` | `[agent] model` / `stream` |
| `KERUX_MAX_ITERATIONS` / `KERUX_MAX_HEALING_ATTEMPTS` | `[agent]` iteration/healing knobs |
| `KERUX_TOOL_TIMEOUT` / `KERUX_REQUEST_TIMEOUT` / `KERUX_CONTEXT_WINDOW` | `[agent]` timeout/window knobs |
| `KERUX_SYSTEM_PROMPT` | `[agent] system_prompt` |
| `KERUX_AUTONOMOUS_*` (`INTERVAL`, `TODO`, `STATUS`, `TEST_COMMAND`, `GIT_REMOTE`, `GIT_BRANCH`, `COMMIT_MESSAGE`, `COMMAND_TIMEOUT`, `MAX_FAILURES`) | `[autonomous]` fields |
| `KERUX_LOG_LEVEL` | `[logging] level` |
| `KERUX_SKILLS_DIR` | `[skills] root_dir` |

Not env-overridable: everything else (edit `kerux.toml`). `KERUX_HOME` relocates the state root (`~/.kerux` by default) but is not a config-file search path.
