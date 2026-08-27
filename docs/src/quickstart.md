# Quickstart

![Kerux workspace](assets/chat.png)

## Build

```bash
git clone https://github.com/eikarna/hermes-rs.git
cd hermes-rs
cargo build --release
```

The binary lands at `target/release/kerux`.

## Configure

Fastest path — the interactive wizard:

```bash
kerux wizard
```

It sniffs provider credentials from the environment, validates the API key
with a live model-list call, lets you fuzzy-pick a model (with capability
badges), optionally probes it and wires a fallback model, then writes the
config and runs a smoke test. `kerux model` switches the model anytime.

Or copy the example config and fill in your provider:

```bash
mkdir -p ~/.config/kerux
cp kerux.example.toml ~/.config/kerux/config.toml
```

Config lookup order: `--config <path>` flag first, then `./kerux.toml`, `./.kerux.toml`, and finally `~/.config/kerux/config.toml` (Unix) / `%APPDATA%\kerux\config.toml` (Windows).

Minimal config:

```toml
[client]
provider = "openai"
base_url = "https://api.openai.com/v1"

[agent]
model = "gpt-4o"
```

Or use environment variables directly:

```bash
export KERUX_PROVIDER=openai
export KERUX_MODEL=gpt-4o
export OPENAI_API_KEY=***
```

API keys resolve from `[client] api_key`, the `OPENAI_API_KEY` environment variable, or an OAuth profile (`kerux auth login nous`, referenced via `[client] auth_ref`). Run `kerux --help` for CLI overrides (`--api-key`, `--base-url`, `--model`, ...).

## Run

Interactive TUI:

```bash
kerux chat
```

Single-shot:

```bash
kerux run --query "explain this codebase"
```

Gateway mode (Telegram + WhatsApp + Discord + Slack):

```bash
kerux serve
```

## Verify

```bash
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```
