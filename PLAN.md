# Integration Plan: Aider + Hermes-RS

**Status:** SHIPPED (Phases 1-5 complete as of 0.1.3 main; deltas vs. spec below)

**Created:** 2026-08-07

---

## Shipped Deltas vs. Plan

Phase-by-phase reality check against the original scope:

- **Phase 1 (provider routing):** shipped, plus per-model capability tables with longest-prefix matching (`lookup_capabilities`), `supports_vision` / `supports_tool_calls` fields, and `<edit_format>` prompt hints routed from advertised capabilities.
- **Phase 2 (repo map):** shipped (tree-sitter C/Python/Rust/TypeScript, personalized PageRank, token-budgeted renderer). Discovery is capped at 500 files before ranking (`discover_source_files_with_limit`). Format follows Aider's outline style; incremental file-watcher ranking not ported.
- **Phase 3 (edit blocks):** shipped (`edit_block` tool, atomic multi-edit, exact+fuzzy sharing `patch` matching, parsed via `parse_edit_blocks`; model-level routing via `EditFormat::SearchReplace`/`Patch`).
- **Phase 4 (git harness):** shipped (`hermes_core::githarness` with snapshot/guard/commit/undo; TUI `/undo`). `commit_transaction` runs per tick in autonomous mode and post-run in the TUI when `[agent].auto_commit = true`.
- **Phase 5 (skill & memory lifecycle):** shipped as `[curator]` policy + non-blocking pass on startup/tick; option `[curator].interval_secs` enables periodic mid-session passes. Memory pinning (`pinned: true`) exempts from decay/prune/dedup and is a serialized MEMORY.md header field. Skill staleness keyed off SKILL.md mtime, not usage telemetry. Skill distillation creates tag-clustered `distilled-<tag>` drafts; `[curator].skill_distill_llm_summary = true` rewrites the body via the active LLM (falls back to bullet list on error). Drafts route to `_pending/` by default (`[curator].auto_approve_skills = false`) and require TUI approval (`a`) before loading. Session archiving is idle-time-only.

**Shipped since original plan:** skill provenance metadata (`SkillOrigin::Agent/User`, `pinned`, `use_count`, `last_activity_at`) in SKILL.md front matter, with provenance-gated auto-archive in the curator.

---

---

## Executive Summary

Integrate Aider's three core capabilities into Hermes-RS to bridge model-agnostic LLM access, intelligent repo indexing, and robust git-driven development workflows:

1. **Repo Map** — AST-based file ranking + Personalized PageRank for token-efficient codebase context
2. **Edit Format** — SEARCH/REPLACE blocks for lean code generation (vs. full-file rewrites)
3. **Git Integration** — Auto-commit with Conventional Commit messages + `/undo` rollback command

**Outcome:** Hermes-RS gains Aider's productivity patterns (model-agnostic backend, edit-efficient workflows) while retaining Hermes-Agent's self-learning loop (skills, memory, curator, cron).

---

## Current State Analysis

### Hermes-RS (Rust, v0.1.3)

**Strengths:**
- Streaming-first ReAct loop with tolerant XML parsing
- Self-healing LLM error recovery
- TUI + autonomous modes with state persistence
- 99+ unit & integration tests

**Gaps:**
- LLM client hardcoded to OpenAI (no Anthropic, Ollama, OpenRouter)
- File context is naive (full reads or line-offset pagination)
- Edit tools: only `patch` and `file_write` (no efficient edit format)

**Memory/Skills:** Basic `memory.rs` with no curator, no trajectory compression, no skill lifecycle.

### Hermes-Agent (Python reference, ~12k LOC run_agent.py)

**Strengths:**
- Self-learning: curator + skill auto-creation
- Memory: FTS5 session search with LLM summarization
- Cron/webhook automation + multi-platform gateway
- Durable skill + memory state across sessions

**Not for direct reuse:** Monolithic codebase; valuable as architectural reference only.

### Aider (Python CLI, paul-gauthier/aider)

**Strengths:**
- Repo map: tree-sitter AST + Personalized PageRank ranking
- Edit format: SEARCH/REPLACE blocks (token-efficient vs. full-file)
- Git harness: dirty-tree protection, Conventional Commit generation, `/undo`
- Model-agnostic: supports 40+ providers via provider abstraction

**Not for adoption:** We extract subsystem design, not copy code.

---

## Integration Phases

### Phase 1: Model-Agnostic Client (Weeks 1-2)

**Goal:** Swap hardcoded OpenAI client for pluggable provider abstraction.

**Scope:**
- Abstract `LLMProvider` trait over `OpenAIClient`
- Implement adapters: OpenAI, Anthropic, Ollama, OpenRouter
- Config extension: provider selection + per-provider settings
- Capability negotiation: max_tokens, edit_format, streaming support

**Key Files:**
- New: `crates/hermes-core/src/client/provider.rs` (trait + routing)
- New: `crates/hermes-core/src/client/providers/*.rs` (per-provider impl)
- Modified: `crates/hermes-core/src/config.rs` (provider config section)
- Modified: `crates/hermes-core/src/agent.rs` (use trait, not hardcoded client)

**Design Pattern:**

```rust
#[async_trait]
pub trait LLMProvider: Send + Sync {
    async fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatResponse>;

    async fn chat_streaming(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatStreamResponse>;

    fn capabilities(&self, model: &str) -> ProviderCapabilities;
}

#[derive(Clone)]
pub struct ProviderCapabilities {
    pub max_input_tokens: usize,
    pub max_output_tokens: usize,
    pub edit_format: EditFormat,  // search_replace, patch, full_file
    pub supports_streaming: bool,
    pub supports_reasoning: bool,
    pub feature_flags: FeatureFlags,
}
```

**Config Extension (TOML):**

```toml
[client]
provider = "openai"  # openai | anthropic | ollama | openrouter

[client.openai]
base_url = "https://api.openai.com/v1"
api_key = "sk-..."
model_mapping = {}

[client.anthropic]
api_key = "sk-ant-..."
model_mapping = {}

[client.ollama]
base_url = "http://localhost:11434"
model_mapping = {}

[client.openrouter]
base_url = "https://openrouter.ai/api/v1"
api_key = "sk-or-..."
model_mapping = {}
```

**Backward Compat:** Default to OpenAI if provider not specified; existing hermes.toml files continue working.

**Verification:**
- All existing tests pass with default (OpenAI) provider
- New provider tests mock HTTP responses (no live API calls)
- CLI invocation still works: `hermes run --query "test"` defaults to OpenAI

---

### Phase 2: Repo Map (Weeks 3-4)

**Goal:** Replace naive file context with Aider-style repo map.

**Scope:**
- Symbol extraction via tree-sitter (defs + refs per file)
- Personalized PageRank scorer (file importance ranking)
- Token-budget binary search (fit ranked tags to context window)
- Render concise file tree with line numbers and key signatures

**Key Files:**
- New: `crates/hermes-core/src/repomap/extractor.rs` (tree-sitter symbol extraction)
- New: `crates/hermes-core/src/repomap/scorer.rs` (PageRank ranking)
- New: `crates/hermes-core/src/repomap/budgeter.rs` (token-budget trimming)
- New: `crates/hermes-core/src/repomap/mod.rs` (public API)
- Modified: `crates/hermes-core/src/agent.rs` (inject `<repo_map>` into system prompt)
- Modified: `crates/hermes-core/src/config.rs` (add `[repomap]` section)

**Algorithm Summary:**

1. **Extract symbols** from all files (tree-sitter AST traversal):
   - Tags: `(rel_path, line, name, kind)` where kind = def / ref
   - Language support: Rust, Python, TypeScript, C (priority order)
   - Cache to disk (sled KV: key = `(path, mtime)` → serialized tags)

2. **Build ranked graph:**
   - Create directed graph: file → file (edges = identifier references)
   - Apply Personalized PageRank with restart bias toward active chat files + mentioned identifiers
   - Per-definition score = aggregated incoming edge rank
   - Sort tags by descending score

3. **Binary-search trim:**
   - Target: token budget (default 1024, scales 8x if no active files)
   - Midpoint candidate: render ranked tags slice, estimate tokens
   - Keep reducing until within 15% tolerance

4. **Render output:**
   - Concise tree format: file paths + line numbers + key definitions
   - Preserve relative indentation for nested symbols

---

### Phase 3: Edit Format (Weeks 5-6)

**Goal:** Add SEARCH/REPLACE block parsing and application.

**Scope:**
- Parser: extract file path, search block, replace block from LLM stream
- Applier: exact match strategy, fuzzy fallback (normalized whitespace), diff-based fuzzy fallback
- Capability routing: inject verbatim format spec into system prompt if model supports SEARCH/REPLACE
- Fallback: legacy patch tool for models without SEARCH/REPLACE support

**Key Files:**
- New: `crates/hermes-core/src/tools/edit_parser.rs` (SEARCH/REPLACE block parser)
- New: `crates/hermes-core/src/tools/edit_applier.rs` (fuzzy diff applier)
- Modified: `crates/hermes-core/src/agent.rs` (system prompt format spec injection)
- Modified: `crates/hermes-core/src/tools/builtin.rs` (register edit tool)

**Verbatim Block Spec:**

```text
Every *SEARCH/REPLACE block* must use this format:
1. The *FULL* file path alone on a line, verbatim.
2. The opening fence and code language, eg: ```rust
3. The start of search block: <<<<<<< SEARCH
4. A contiguous chunk of lines to search for in the existing source code
5. The dividing line: =======
6. The lines to replace into the source code
7. The end of the replace block: >>>>>>> REPLACE
8. The closing fence: ```
```

**Application Strategy:**

1. **Exact match (first hit):** Find literal search string in file content and replace.
2. **Relative Indentation Strategy:** If exact match fails, normalize whitespace per line, locate match, apply replacement preserving file's indentation.
3. **Fuzzy Search Fallback:** Use `similar` crate diff engine to locate near-match line spans and apply edits.
4. **Validation:** Reject empty-to-empty blocks; log error if search block not found in target file.

**Verification:**
- Unit tests: parser against Aider output formats
- Integration tests: exact and fuzzy replacements across multi-line blocks
- E2E tests: full agent turn with block edits on sample codebase

---

### Phase 4: Git Integration (Weeks 7-8)

**Goal:** Transactional git workflow with auto-commit and rollback.

**Scope:**
- Pre-edit dirty tree check (detect unstaged/staged changes before applying edits)
- Post-edit Conventional Commit generation (call weak model with diff summary)
- Subcommand `/undo` (revert last agent commit safely)
- Integration into autonomous coding loop

**Key Files:**
- New: `crates/hermes-cli/src/git_harness.rs` (git transactions, Conventional Commit generation, undo)
- Modified: `crates/hermes-cli/src/autonomous.rs` (delegate git operations to git harness)
- Modified: `crates/hermes-cli/src/main.rs` (register `/undo` subcommand)

**Workflow Sequence:**

1. **Pre-Edit Check:**
   - Run `git status --porcelain` to detect uncommitted user changes
   - If dirty and `auto_commit_dirty` enabled: commit dirty files with message `"committing dirty files before agent changes"`

2. **Edit Application:**
   - Apply SEARCH/REPLACE or patch edits
   - Run validation command (`cargo test --workspace`)

3. **Post-Validation Auto-Commit:**
   - If validation passes: stage edited files (`git add <files>`)
   - Invoke weak LLM model (e.g. `gpt-4o-mini` or `haiku`) with `git diff --cached` to generate Conventional Commit message
   - Commit: `git commit -m "<message>"`
   - Append commit hash to `autonomous-status.toml` history ledger

4. **Rollback (`/undo`):**
   - Check if HEAD commit was created by agent (verify hash in ledger)
   - Safety check: reject if commit has multiple parents (merge) or is pushed (`HEAD` == `origin/<branch>`)
   - Revert: `git checkout HEAD~1 -- <files>` + `git reset --soft HEAD~1`
   - Print confirmation: `Removed: <hash> <commit_message>`

**Verification:**
- Unit tests: git status parser, Conventional Commit prompt construction
- Integration tests: git harness on temporary git repository (commit, rollback, dirty-tree protection)
- E2E test: autonomous loop executing multi-step task with rollback on test failure

---

### Phase 5: Skills & Memory Lifecycle (Weeks 9-10)

**Goal:** Adapt Hermes-Agent self-learning (curator + skill auto-creation) to Rust.

**Scope:**
- Extend skill metadata (`created_by`, `usage_count`, `last_activity`, `pinned`, `state`)
- Curator background task (scan agent-created skills, archive stale skills after threshold)
- Async skill distillation (extract reusable skills from completed agent runs)
- Skill search & memory integration

**Key Files:**
- New: `crates/hermes-core/src/curator.rs` (curator review loop & skill archiving)
- Modified: `crates/hermes-core/src/skills.rs` (extend skill metadata and lifecycle state)
- Modified: `crates/hermes-core/src/distillation.rs` (add skill creation pass after run)
- Modified: `crates/hermes-cli/src/main.rs` (add `hermes curator` subcommands)

**Curator Rules:**
- Only touch skills with `created_by = "agent"` provenance
- Never hard-delete; max destructive action is archive (`~/.hermes/skills/.archive/`)
- Pinned skills (`pinned = true`) are exempt from auto-archiving and review passes
- Usage tracking: record `use_count`, `last_activity_at` per skill invocation

**Verification:**
- Unit tests: curator transition rules, skill metadata serialization
- Integration tests: skill creation from mock conversation history
- E2E test: agent creates skill → curator archives after simulated time elapsed

---

## Architectural Invariants

1. **Zero Prompt Cache Invalidation:** System prompt and tool schemas must remain stable during a conversation. Configuration changes take effect on next session.
2. **Minimal Footprint:** No extra dependencies unless stdlib/existing dependencies cannot solve the problem.
3. **Graceful Fallback:** If a model doesn't support SEARCH/REPLACE, degrade to `patch` tool; if repo map fails, degrade to full-file loading.
4. **Git Safety:** Never commit unvalidated edits; never undo user-made commits; always check dirty tree state before modifying files.
5. **Standard Output & Verification:** Every phase requires unit tests, integration tests, and explicit verification against `cargo check` and `cargo test`.
