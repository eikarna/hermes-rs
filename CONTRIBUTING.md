# Contributing to Hermes-RS

Thank you for your interest in contributing to Hermes-RS! This document outlines the process and conventions for contributing.

## Table of Contents

- [Getting Started](#getting-started)
- [Development Setup](#development-setup)
- [Coding Conventions](#coding-conventions)
- [Testing](#testing)
- [Submitting Changes](#submitting-changes)
- [Reporting Issues](#reporting-issues)

## Getting Started

### Prerequisites

- **Rust 1.86+** (MSRV — enforced by CI)
- **Git** 2.40+
- A code editor with Rust-analyzer support (recommended)

### Fork and Clone

```bash
git clone https://github.com/YOUR_USERNAME/hermes-rs.git
cd hermes-rs
```

## Development Setup

```bash
# Build all crates
cargo build --workspace

# Run all tests
cargo test --workspace

# Run clippy (must pass with zero warnings)
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Check formatting
cargo fmt --all --check

# Build release binary
cargo build --workspace --release
```

### Project Structure

```
hermes-rs/
├── crates/
│   ├── hermes-core/       # Core library (agent loop, tools, client, parser)
│   │   └── src/
│   │       ├── agent.rs       # ReAct orchestration loop
│   │       ├── client.rs      # OpenAI API client + SSE streaming
│   │       ├── context.rs     # Context window management
│   │       ├── error.rs       # Error types
│   │       ├── gateway.rs     # Platform adapters (Telegram, Discord, Slack)
│   │       ├── mcp.rs         # MCP protocol client (HTTP + stdio)
│   │       ├── memory.rs      # Persistent file-backed memory
│   │       ├── parser.rs      # Tolerant XML tool-call parser
│   │       ├── platform.rs    # Cross-platform utilities
│   │       ├── schema.rs      # JSON Schema generation
│   │       ├── skills.rs      # Skills management system
│   │       ├── tools.rs       # Tool registry + trait
│   │       ├── tools/         # Built-in tool implementations
│   │       └── trajectory.rs  # RL trajectory export
│   └── hermes-cli/        # CLI binary
│       └── src/
│           └── main.rs        # CLI entry point + subcommands
├── Cargo.toml             # Workspace root
└── .github/workflows/     # CI/CD pipelines
```

## Coding Conventions

### Rust Style

- **Follow `rustfmt`**: All code must pass `cargo fmt --all --check`. No exceptions.
- **Follow Clippy**: Zero clippy warnings with `-D warnings`. Fix warnings before pushing.
- **Edition 2021**: The workspace uses Rust edition 2021.
- **No unstable features**: Code must compile on the MSRV (1.86) without nightly-only features.

### Naming

| Item | Convention | Example |
|------|-----------|---------|
| Crates | `kebab-case` | `hermes-core`, `hermes-cli` |
| Types/Structs/Enums | `PascalCase` | `AgentConfig`, `ToolRegistry` |
| Functions/Methods | `snake_case` | `register_builtin_tools` |
| Constants | `SCREAMING_SNAKE` | `MAX_RETRY_ATTEMPTS` |
| Module files | `snake_case.rs` | `terminal_tool.rs` |
| Tool trait impls | `PascalCase + Tool` suffix | `EchoTool`, `CalculatorTool` |

### Architecture Patterns

**Tool Implementation**: All tools implement the `HermesTool` trait:

```rust
#[async_trait]
impl HermesTool for MyTool {
    fn name(&self) -> &str { "my_tool" }
    fn description(&self) -> &str { "Description for LLM" }
    fn schema(&self) -> ToolSchema {
        #[derive(JsonSchema, Deserialize)]
        #[serde(rename_all = "camelCase")]
        #[allow(dead_code)]
        struct Args { field: String }
        ToolSchema::from_type::<Args>("my_tool", "Description")
    }
    async fn execute(&self, args: Value, _ctx: ToolContext) -> ToolResult {
        ToolResult::success("my_tool", json!({ "result": "ok" }))
    }
}
```

**Error Handling**: Use `crate::error::Error` for library code, `anyhow::Result` for CLI/application code. Never panic in library code.

**Async Runtime**: Never call `tokio::runtime::Runtime::block_on()` inside an async context. All async functions must be `.await`ed directly. This is the most common source of runtime panics.

**Cross-Platform**: Use `platform.rs` utilities for shell detection, config directories, and file permissions. Never hardcode Unix or Windows paths.

**Schema Structs**: Structs used only for JSON Schema generation must have `#[allow(dead_code)]` since their fields are never directly read.

### Comments

- Write **no comments** unless the code is genuinely non-obvious.
- Prefer self-documenting code: good names, small functions, clear types.
- `// TODO:` comments are acceptable for known incomplete work.

### Imports

- Group imports in this order: `std`, external crates, `crate`/`super`.
- Use `use` blocks to group related imports.
- Never use wildcard imports (`use crate::*`) except in test modules.

## Testing

### Requirements

- All new features **must** include tests.
- All bug fixes **must** include a regression test.
- Tests must pass on all three platforms: Linux, macOS, Windows.

### Test Conventions

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_my_feature() {
        // Arrange
        let sut = MyStruct::new();

        // Act
        let result = sut.do_thing().await;

        // Assert
        assert!(result.is_ok());
    }
}
```

- Use `#[tokio::test]` for async tests.
- Use unique temp directory names with atomic counters/timestamps to avoid race conditions in parallel test execution.
- Test modules live in the same file as the code they test, inside `#[cfg(test)] mod tests { ... }`.

### Running Tests

```bash
# All tests
cargo test --workspace

# Specific test
cargo test -p hermes-core test_parser

# With output
cargo test --workspace -- --nocapture

# Release mode (catches different optimizations)
cargo test --workspace --release
```

## Submitting Changes

### Branch Naming

| Type | Format | Example |
|------|--------|---------|
| Feature | `feat/description` | `feat/web-search-tool` |
| Bug fix | `fix/description` | `fix/parser-malformed-xml` |
| Refactor | `refactor/description` | `refactor/error-types` |
| Docs | `docs/description` | `docs/api-reference` |
| CI | `ci/description` | `ci/add-arm-builds` |

### Commit Messages

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>

[optional body]

[optional footer]
```

Types: `feat`, `fix`, `refactor`, `docs`, `test`, `ci`, `chore`, `perf`

Examples:
```
feat(tools): add web search tool with DuckDuckGo scraper
fix(parser): handle unclosed JSON in tool call arguments
refactor(client): extract SSE parsing into dedicated stream type
ci(build): add cross-compilation for ARM64 and musl targets
```

### Pull Request Process

1. **Create a feature branch** from `main`.
2. **Make your changes** following the coding conventions above.
3. **Add tests** for new functionality or bug fixes.
4. **Run the full CI suite locally**:
   ```bash
   cargo fmt --all --check
   cargo clippy --workspace --all-targets --all-features -- -D warnings
   cargo test --workspace
   cargo doc --workspace --no-deps
   ```
5. **Push and open a PR** against `main`.
6. **Fill in the PR template** completely.
7. **Ensure CI passes** — all checks must be green before merge.
8. **Request review** from a maintainer.
9. **Address review feedback** with new commits (do not force-push during review).

### PR Size Guidelines

- **Small (< 300 lines)**: Ideal. Quick to review, easy to understand.
- **Medium (300–800 lines)**: Acceptable for cohesive features. Split if possible.
- **Large (> 800 lines)**: Break into smaller PRs unless it's a single cohesive change that can't be split.

## Reporting Issues

### Bug Reports

Use the **Bug Report** template. Include:

1. **Hermes-RS version**: `hermes --version` or git commit hash
2. **Rust version**: `rustc --version`
3. **OS and architecture**: e.g., `aarch64-linux-android`, `x86_64 Windows`
4. **Steps to reproduce**: Minimal reproducer
5. **Expected vs actual behavior**
6. **Logs**: With `RUST_BACKTRACE=1` and `--log-level debug`

### Feature Requests

Use the **Feature Request** template. Include:

1. **Use case**: What problem does this solve?
2. **Proposed solution**: How should it work?
3. **Alternatives considered**: What else did you look at?
4. **References**: Links to relevant docs, the Python hermes-agent, etc.

## Release Process (Maintainers)

1. Update `version` in all `Cargo.toml` files.
2. Update `CHANGELOG.md`.
3. Tag: `git tag v0.x.y && git push --tags`.
4. CI builds release binaries for all targets automatically.

## License

By contributing, you agree that your contributions will be licensed under both the [MIT License](LICENSE-MIT) and the [Apache License 2.0](LICENSE-APACHE).
