# Quickstart

## Build

```bash
git clone https://github.com/eikarna/hermes-rs.git
cd hermes-rs
cargo build --release
```

The binary lands at `target/release/kerux`.

## Configure

Copy the example config and fill in your provider:

```bash
cp kerux.example.toml ~/.config/kerux/kerux.toml
```

Minimal config:

```toml
[client]
provider = "openai"
model = "gpt-4o"

[client.auth]
api_key_env = "OPENAI_API_KEY"
```

Or use environment variables directly:

```bash
export KERUX_PROVIDER=openai
export KERUX_MODEL=gpt-4o
export OPENAI_API_KEY=***
```

## Run

Interactive TUI:

```bash
kerux
```

Single-shot:

```bash
kerux run "explain this codebase"
```

Gateway mode (Telegram + WhatsApp):

```bash
kerux serve
```

## Verify

```bash
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```
