# Hermes-RS Development Progress & Handoff Document

**Last Updated:** August 10, 2024  
**Current Version:** v0.1.3 (main branch)  
**Status:** ✅ All Aider Phases Complete - Stable Release Candidate

---

## 📊 Current State Summary

### Implemented Features (Phases 1-5 Complete)

| Phase | Feature | Status | Commit Hash | Notes |
|-------|---------|--------|-------------|-------|
| **Phase 1** | Model-agnostic provider routing + capability tables | ✅ Done | `aa53ed6` | OpenAI, Anthropic, Ollama, OpenRouter adapters; per-model metadata |
| **Phase 2** | Tree-sitter AST repo map extraction + PageRank scoring | ✅ Done | `b141e7d`, `bf25a0a` | C/Python/Rust/TypeScript support; capped at 500 files to prevent stalls |
| **Phase 3** | Aider-style SEARCH/REPLACE (`edit_block`) + capability routing | ✅ Done | `a697180` | Atomic multi-edit tool with exact+fuzzy matching |
| **Phase 4** | Transactional git harness | ✅ Done | `2e7a75a` | Snapshots, dirty-tree protection, Conventional Commits, `/undo` command |
| **Phase 5** | Skill & memory lifecycle management | ✅ Done | `aa53ed6`, `a309db0`, `2a4485a` | Curator passes, decay/prune/dedup, distillation, archival, pinning, LLM prose summarization |

### Test Coverage
- **hermes-core**: 235 tests passing (1 pre-existing env failure: MCP stdio requires Python binary)
- **hermes-cli**: 104 tests passing
- **Repomap-specific**: 7 tests all passing
- **Clippy**: Clean (`--all-targets --all-features -- -D warnings`)

---

## 🎯 Next Steps - Priority Order

Based on TODO.md tracking and natural progression after Aider integration, here are recommended follow-ups in priority order:

### High Priority (User Impact)

#### 1. ~~Trajectory Compression~~ (SHIPPED, fact-level)
`[curator].compression_min_age_days` (default 60), `compression_max_importance` (90), `compression_min_count` (5). Deterministic fold of old, low-importance, unpinned `fact` blocks into a single `session_summary` per curator pass — no LLM, no token spend. Distilled (importance 90) and pinned facts are exempt. Inter-session *message* compression was scoped out: `MemoryManager` persists blocks, not transcripts (see `memory.rs` — sessions carry metadata only); lifting that ceiling is future work.

---

#### 2. ~~Auto-commit Wiring~~ (SHIPPED, run-level)
`[agent].auto_commit = true` auto-commits a successful interactive run's working-tree changes via `GitHarness::commit_transaction` (Conventional Commit derived from staged diff). Wired at TUI run completion (`tui/app.rs` `finish_run_if_ready`) rather than per-tool call: run-level commits respect `/undo` as the intermediate rollback and batch all of a run's edits into one commit. Autonomous mode already committed per tick.

---

#### 3. Gemini Adapter (Provider Expansion)
**Why:** Gemini is major competitor with strong performance/money ratio; users expect parity across providers.

**What to do:**
- Implement `GeminiClient` struct with streaming/non-streaming chat paths
- Map capabilities table rows for Gemini models (gemini-pro, gemini-advanced, etc.)
- Handle Gemini's specific API format (REST endpoint differences from OpenAI-compatible)
- Support function calling equivalent (Gemini uses same JSON schemas)
- Update docs with examples and cost tables

**Implementation hints:**
- Base structure on `OpenAIClient` since Gemini has REST-compatible patterns
- Endpoint: `https://generativelanguage.googleapis.com/v1/models/{model}:generateContent`
- Key differences: needs API key in header/query param, response wrapper different
- Tools schema same as OpenAI, so reuse existing `ToolSchema` types

**Files to modify:**
- `crates/hermes-core/src/client/`: New `gemini.rs` module
- `crates/hermes-core/src/client.rs`: Add `ProviderKind::Gemini` variant
- `crates/hermes-core/src/client/provider.rs`: Capability rows for Gemini models
- `crates/hermes-cli/src/main.rs`: Runtime client factory for Gemini

**Tests needed:**
- Mock server tests for generateContent endpoint
- Streaming delta parsing matches Gemini SSE format
- Capability lookup returns correct values for Gemini model names

**ETA:** ~6-8 hours engineering effort

---

### Medium Priority (Quality/UX)

#### 4. ~~Skill Approval Flow~~ (SHIPPED)
Distilled drafts default to `<skills>/_pending/` (`[curator].auto_approve_skills = false`), never auto-load, and appear in the TUI Skills panel with a `pending` badge. `a` approves (moves to loadable root, refreshes), `d` discards. Set `auto_approve_skills = true` to restore immediate load.

---

#### 5. ~~Per-Model Edit Format Override~~ (SHIPPED)
`[agent].edit_format_override` (`search_replace` | `patch` | `full_file`) implemented in `config.rs` + applied at prompt-hint generation in `agent.rs`; invalid values rejected by TOML parse error via serde.

---

### Low Priority (Nice-to-have)

#### 6. Vision/Image Input Support
**Why:** Multimodal agents gain significant capabilities (document scanning, screenshot understanding).

**Status:** ProviderCapabilities added `supports_vision` field but no implementation exists yet.

**Work needed:** Minimal framework setup, heavy integration work:
- Image preprocessing pipeline (resize, encode, base64)
- Multimodal request construction per-provider
- Tool-call restrictions (vision-only contexts disable some tools)

**ETA:** ~8-12 hours per major provider (Anthropic/Ollama first)

---

## 🧪 Known Issues & Technical Debt

### Pre-existing Environment Failure
- **Issue:** `mcp::tests::stdio_client_connects_lists_and_calls_tool` fails requiring `python` binary
- **Impact:** None in production use case (MCP stdio just optional feature)
- **Fix required:** Install Python on test runners or mock python dependency in tests

### Performance Edge Cases
- **Huge repos (>6k files):** Now protected via file capping but discovery still scans entire tree first. Optimization: early-return scan once cap hit.
- **Tree-sitter parsing on very large files (>1MB):** Parser may stall. Recommendation: implement file size threshold warning in extractor.

### Missing Documentation
- **TODO:** Update AGENTS.md with new features documentation
- **TODO:** Add usage examples to README for [curator] configuration
- **TODO:** Write RFC for trajectory compression design before implementation

---

## 📁 File Locations Reference

| Component | Primary Files | Purpose |
|-----------|---------------|---------|
| **Repo Map** | `crates/hermes-core/src/repomap/extractor.rs`, `scorer.rs`, `budgeter.rs` | Symbol extraction, PageRank ranking, token-budgeted rendering |
| **Git Harness** | `crates/hermes-core/src/githarness.rs`, `crates/hermes-cli/src/tui/app.rs` (`/undo`) | Snapshot/restore, dirty-tree protection, Conventional Commits |
| **Curator** | `crates/hermes-core/src/curator.rs` | Decay/prune/dedup/archive/distillation loops |
| **Skills** | `crates/hermes-core/src/skills.rs`, `crates/hermes-core/src/repomap/budgeter.rs` | SKILL.md front matter loading, metadata persistence, archive management |
| **Config** | `crates/hermes-core/src/config.rs`, `hermes.example.toml` | TOML-based settings runtime config resolution |
| **Providers** | `crates/hermes-core/src/client/anthropic.rs`, `openai.rs`, `ollama.rs`, `openrouter.rs` | LLM provider implementations with streaming normalization |
| **Agents** | `crates/hermes-core/src/agent.rs`, `crates/hermes-cli/src/tui/app.rs` | ReAct loop orchestration, system prompt construction |

---

## 🚀 Quick Start for New Contributor

1. **Clone repository:**
   ```bash
   git clone https://github.com/yourservice/hermes-rs.git
   cd hermes-rs
   ```

2. **Install dependencies:**
   ```bash
   # Rust toolchain (Rustup recommended)
   rustup update stable && cargo update
   
   # Python (for MCP tests only - optional)
   sudo apt install python3  # Ubuntu/Debian
   ```

3. **Run tests:**
   ```bash
   cargo test --workspace
   ```
   Expected: 235 core tests + 104 CLI tests = 339 total (1 known skip/failure)

4. **Build binary:**
   ```bash
   cargo build --release
   target/release/hermes
   ```

5. **Start coding:**
   - Pick task from "Next Steps - Priority Order" above
   - Read relevant files listed under "What to do"
   - Write failing test first (TDD approach)
   - Run `cargo clippy` during development
   - Submit PR referencing this progress doc

---

## 📞 Contact Information

**Primary Maintainers:** [List maintainers here]  
**Slack Channel:** #[channel-name](link)  
**GitHub Issues:** [Link to issues page]  

**Handoff Instructions:** When handing off project maintenance or transitioning team members, share this PROGRESS.md along with:
- Full test suite results (`cargo test --workspace`)
- Recent commit history (`git log --oneline -10`)
- Current open pull requests (`gh pr list`)
- Access tokens/secrets rotation schedule

---

*Document generated: 2024-08-10*  
*Repository main branch HEAD: bf25a0a*  
*Branch ahead of origin/main by 10 commits*
