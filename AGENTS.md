# AGENTS.md

## Hermes-RS Project Context

- Current release line: `0.1.3`
- Runtime config is TOML-first and shared through `crates/hermes-core/src/config.rs`
- Rich CLI/TUI uses `ratatui` and lives under `crates/hermes-cli/src/tui/`
- Autonomous coding mode lives in `crates/hermes-cli/src/autonomous.rs` and is launched through `hermes autonomous` or `hermes run --autonomous`
- Repo-root `TODO.md` is the task ledger for autonomous mode; keep `Implemented` and `Pending` accurate when autonomous behavior changes
- The workspace view has `Conversation`, `Reasoning`, `Activity`, and management panels for `MCP`, `Skills`, and `Behavior`
- When config fields change, update `hermes.example.toml` in the repo root in the same change
- When user-facing behavior changes, update `README.md`, `CHANGELOG.md`, and screenshots in `assets/` if the UI changed materially
- Tagged releases are created from `CHANGELOG.md`: push `vX.Y.Z`, then GitHub Actions builds artifacts and publishes the GitHub Release from the matching changelog section
- Preferred verification commands:
  - `cargo fmt --all`
  - `cargo check --workspace`
  - `cargo test --workspace`
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`

Behavioral guidelines to reduce common LLM coding mistakes. Merge with project-specific instructions as needed.

**Tradeoff:** These guidelines bias toward caution over speed. For trivial tasks, use judgment.

## 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:
- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

## 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

## 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:
- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:
- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

## 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:
- "Add validation" → "Write tests for invalid inputs, then make them pass"
- "Fix the bug" → "Write a test that reproduces it, then make it pass"
- "Refactor X" → "Ensure tests pass before and after"

For multi-step tasks, state a brief plan:
```
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Strong success criteria let you loop independently. Weak criteria ("make it work") require constant clarification.

---

**These guidelines are working if:** fewer unnecessary changes in diffs, fewer rewrites due to overcomplication, and clarifying questions come before implementation rather than after mistakes.
