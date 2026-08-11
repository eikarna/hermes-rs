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

#### 1. Trajectory Compression (Memory Management)
**Why:** MEMORY.md can grow large over long sessions, increasing context cost and slowing retrieval.

**What to do:**
- Add compression pass that collapses old session messages into distilled facts
- Preserve semantic meaning while reducing token count by 70-90%
- Integrate with curator run: `curator.compress_sessions(age_days_threshold)`
- Output: `<session_summary>` blocks in MEMORY.md with timestamps

**Implementation hints:**
- Use existing `distill_session_to_memory()` logic but operate backwards from oldest sessions
- Respect `[agent].context_window` and prioritize recent+important content
- Write compressed summaries as separate "compressed" type memories
- Test: ensure no critical info loss in regression testing

**Files to modify:**
- `crates/hermes-core/src/memory.rs`: Add `compress_sessions(age_days: u64)` method
- `crates/hermes-core/src/distillation.rs`: Reuse fact extraction logic for compression
- `crates/hermes-core/src/curator.rs`: Add `memory_compression_days: usize` config option

**Tests needed:**
- Session compression preserves semantic meaning
- Old uncompressed entries remain queryable
- Memory file size reduction metrics

**ETA:** ~4-6 hours engineering effort

---

#### 2. Auto-commit Wiring (Git Integration Deepening)
**Why:** Currently agent must manually run `/undo` or external tools commit changes. Seamless git workflow improves UX dramatically.

**What to do:**
- Wire `commit_transaction()` call automatically when `edit_block` or `patch` tool succeeds
- Generate meaningful message based on edits made (reuse existing heuristic)
- Handle edge cases: conflicts, staged changes, merge commits
- Optional: configurable policy (always-commit vs confirm vs skip)

**Implementation hints:**
- In `patch_tool::execute()` / `edit_block_tool::execute()`, after successful writes:
  ```rust
  if let Ok(Some(commit_hash)) = harness.commit_transaction(&format!("Edit: {}", summary)) {
      // Optionally notify user or store hash in state
  }
  ```
- Consider adding `git_auto_commit: bool` to `[agent]` config
- Track commit hashes in conversation history for audit trail

**Files to modify:**
- `crates/hermes-core/src/tools/patch_tool.rs`: Call `commit_transaction()` post-success
- `crates/hermes-core/src/tools/edit_block_tool.rs`: Same, plus summarize edits for message
- `crates/hermes-core/src/githarness.rs`: Add helper for message generation from edit diffs
- `crates/hermes-core/src/config.rs`: Add `auto_commit: bool` to BehaviorSettings

**Tests needed:**
- Automatic commit generates correct conventional message
- Fallback gracefully when no git repo or uncommitted work
- Conflicts handled cleanly

**ETA:** ~3-4 hours engineering effort

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

#### 4. Skill Approval Flow (Developer Control)
**Why:** Distilled skills are auto-created and loaded immediately without human review, potentially polluting skills directory.

**What to do:**
- Show new distilled skills in TUI Skills panel before they're loadable
- Add `review`/`approve`/`discard` actions
- Persist decisions in `.hermes/skills/.approved` metadata file
- Only load skills after explicit approval (opt-in behavior)

**Implementation hints:**
- Add skill review state machine in `SkillManager`
- Create pending skills subdirectory: `.hermes/skills/pending/<skill_name>`
- Move approved skills to actual skills dir, discard deletes
- UI updates in TUI skills panel: show star/flag icons indicating pending status

**Files to modify:**
- `crates/hermes-core/src/skills.rs`: Add `pending` mode, `approve()`, `discard()` methods
- `crates/hermes-core/src/tui/skills_panel.rs`: Render pending skills list with action buttons
- `crates/hermes-core/src/curator.rs`: Change distillation path to write to pending first
- `crates/hermes-core/src/config.rs`: Add `auto_approve_skills: bool` default false

**Tests needed:**
- Pending skills don't load until approved
- Approved skills persist across restarts
- Discard removes both code and metadata

**ETA:** ~4-5 hours engineering effort

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
