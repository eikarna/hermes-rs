# Changelog
All notable changes to this project will be documented in this file. 
format based on [Keep Changelog](https://keepachangelog.com/en/1.1.0/), this project adheres 
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `[agent].auto_commit` option: when `true` a successful interactive run auto-commits working-tree changes via the git harness's Conventional Commit derivation (default `false`, keeping `/undo`-only behavior)
- `[agent].auto_commit` auto-commits a successful interactive run's working-tree changes with the git harness's derived Conventional Commit (default `false`; `/undo` still snapshots pre-run state)
- `[agent].edit_format_override` (`search_replace` | `patch` | `full_file`) forces the `<edit_format>` prompt hint regardless of the capability-table guess for the configured model
- Model-agnostic provider routing `LLMProvider` trait, capability metadata, `[client].provider` selection, native adapters OpenAI, Anthropic, Ollama, OpenRouter
- Native Anthropic Messages API adapter `x-api-key` / `anthropic-version` headers, `/messages` endpoint routing, native tool schemas, SSE streaming normalization
- Provider-specific endpoint overrides under `[client.anthropic]`, `[client.ollama]`, `[client.openrouter]`, `[client.openai]`; environment overrides via `HERMES_PROVIDER` plus per-provider `*_API_KEY` vars
- Provider-aware session distillation sub-agent delegation so background work follows configured provider
- Autonomous coding mode through `hermes autonomous` `hermes run --autonomous` compatibility alias
- Shared `[autonomous]` runtime configuration autonomous polling interval, TODO path, status report path, validation command, git target, commit message, command timeout, repeated-failure pause threshold
- Repo-root `TODO.md` task ledger `Implemented` `Pending` sections autonomous workspace planning
- Repo-local `autonomous-status.toml` status reports capture autonomous state, validation results, failure summaries, last push targets
- Disposable-repo autonomous validation coverage exercises full tick loop without live model call
- Long-term memory injection into agent system prompts via `<long_term_memory>` context built durable `MEMORY.md` facts
- Async state distillation extracts durable session facts into repo-local `MEMORY.md` after completed agent runs
- Workspace context-file auto-loading `AGENTS.md`, `CLAUDE.md`, `.hermes.md`, `HERMES.md`, prompt-injection scanning child-agent ReAct Hermes-supported provider-specific TUI context-token auto-compaction Prompt-prefixed TUI Claude/Anthropic-style SSE tool-use OpenAI-compatible streaming chunks
- Tree-sitter repo map AST-based file ranking personalized PageRank token-efficient codebase context (`[agent].repo_map_tokens`)
- Aider-style SEARCH/REPLACE blocks lean code generation (vs. full-file rewrites) via `edit_block` tool; model routing hints `EditFormat::SearchReplace/Patch` from capabilities table
- Repo-map file capping via `[agent].repo_map_max_files` (defaults to 500 files) prevents ranking stalls on very large repositories

- Git harness pre-run snapshots dirty-tree protection Conventional Commit derivation from staged diffs `commit_transaction`, `/undo` rollback command
- Lifecycle curator `hermes_core::curator` background pass memory importance decay near-duplicate pruning session auto-archiving stale skill archiving into `_archive/` tag-clustered distillation long-term facts draft skills; runs non-blockingly every agent startup and autonomous tick
- Memory pinning `pinned: true` flag `MemoryManager::set_pinned` MEMORY.md roundtrip curator exemption from decay/prune/dedup; pinned skills exempt archival
- Optional LLM-assisted skill summarization `skill_distill_llm_summary` + `curate_with_llm` rewrites distilled drafts as prose fails back bullet lists CLI runtime client passed when enabled
- Periodic mid-session passes `[curator].interval_secs` spawned once per process first tick delayed so startup/tick passes don't double up
- **Skill provenance metadata**: `SkillOrigin::Agent/User`, `Skill.pinned`, `use_count`, `last_activity_at` parsed/written SKILL.md front matter; curator auto-archive only touches agent-created unpinned skills; `SkillManager.record_use()` / `update_metadata()` persist fields atomically
