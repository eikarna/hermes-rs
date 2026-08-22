# Changelog
All notable changes to this project will be documented in this file. 
format based on [Keep Changelog](https://keepachangelog.com/en/1.1.0/), this project adheres 
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Discord adapter** (`[gateway] discord_enabled` + `discord_token`): REST integration against `discord_api_base` (default `https://discord.com/api/v10`) — bot-token verification via `/users/@me`, message send via `/channels/{id}/messages`, message-create event parsing
- **Slack adapter** (`[gateway] slack_enabled` + `slack_token`): REST integration against `slack_api_base` (default `https://slack.com/api`) with token verification, `send_message`, and update-event parsing
- **Flight recorder** (Tasks 1.x): hash-chained append-only run journals under `~/.kerux/runs/` with a bounded `[recorder]` policy (`record_content`, `record_reasoning`, `max_payload_bytes`, `failure_mode = warn|fail`); git checkpoint evidence and tool-approval decisions journaled per run; read-only `kerux runs list|inspect|verify` CLI; offline-verifiable scrubbed HTML proof-capsule export; Windows-safe journal storage (per-file byte-range locking, staging-dir publish)
- **Deterministic project validators** (Task 2.1): `[validation]` section with `enabled`, `fail_fast`, and `[[validation.validators]]` command specs (`name`, `command`, `required`, `timeout_secs`)
- **Validator execution engine** (Task 2.2): `run_validation_pass` executes declared validators workspace-confined (lexical normalize + canonical containment, symlink-safe) with bounded/redacted output capture, per-spec timeout, fail-fast semantics with skipped results, and every outcome journaled as validation evidence (CLI wiring pending)
- **Edit-protocol metrics** (Task 2.3/2.4): first-pass vs repaired edit outcomes tracked and journaled per run, plus `[agent] max_repair_attempts` bounded repair policy (`None` falls back to `max_healing_attempts`)
- **Static edit-format fallback ladder** (Task 2.5): classified edit-application failures promote a one-way `search_replace → patch → full_file` hint for the rest of the run; an explicit `[agent] edit_format_override` always wins and the hint never demotes mid-run

### Fixed

- Windows journal storage: close event files before renaming the staging dir, lock a sentinel byte instead of the whole file, acquire the event-file lock after publishing the run dir, avoid append-mode event files ("Access is denied")

## [0.2.0] - 2026-08-11

### Added
- **Trajectory compression**: `[curator].compression_min_age_days` (default 60), `compression_max_importance` (90), `compression_min_count` (5) — old, low-importance, unpinned `fact` blocks fold into one deterministic `session_summary` per curator pass, keeping `MEMORY.md` lean without LLM cost
- **Skill approval flow**: `[curator].auto_approve_skills = false` (default) routes distilled draft skills to `<skills>/_pending/` (never auto-loaded); the TUI Skills panel shows them with a `pending` badge — `a` approves into loadable, `d` discards
- `[agent].auto_commit` option: when `true` a successful interactive run auto-commits working-tree changes via the git harness's Conventional Commit derivation (default `false`, keeping `/undo`-only behavior)
- `[agent].edit_format_override` (`search_replace` | `patch` | `full_file`) forces the `<edit_format>` prompt hint regardless of the capability-table guess for the configured model
- Model-agnostic provider routing `LLMProvider` trait, capability metadata, `[client].provider` selection, native adapters OpenAI, Anthropic, Ollama, OpenRouter
- **Gemini adapter** (`[client] provider = "gemini"` or `"google"`): native `generateContent`/`streamGenerateContent?alt=sse` wiring with `x-goog-api-key`-equivalent `?key=` auth, `systemInstruction`/`contents`/`functionDeclarations` translation, `functionCall`↔`tool_calls` mapping, capability rows for gemini-2.5-pro/flash (1M ctx, 64K out, SearchReplace) plus generic `gemini-` fallback (Patch), and `GEMINI_API_KEY`/`GEMINI_BASE_URL` env overrides. Streaming currently one-shot (module `ponytail` note)
- Native Anthropic Messages API adapter `x-api-key` / `anthropic-version` headers, `/messages` endpoint routing, native tool schemas, SSE streaming normalization
- Provider-specific endpoint overrides under `[client.anthropic]`, `[client.ollama]`, `[client.openrouter]`, `[client.openai]`; environment overrides via `KERUX_PROVIDER` plus per-provider `*_API_KEY` vars
- Provider-aware session distillation sub-agent delegation so background work follows configured provider
- Autonomous coding mode through `kerux autonomous` `kerux run --autonomous` compatibility alias
- Shared `[autonomous]` runtime configuration autonomous polling interval, TODO path, status report path, validation command, git target, commit message, command timeout, repeated-failure pause threshold
- Repo-root `TODO.md` task ledger `Implemented` `Pending` sections autonomous workspace planning
- Repo-local `autonomous-status.toml` status reports capture autonomous state, validation results, failure summaries, last push targets
- Disposable-repo autonomous validation coverage exercises full tick loop without live model call
- Long-term memory injection into agent system prompts via `<long_term_memory>` context built durable `MEMORY.md` facts
- Async state distillation extracts durable session facts into repo-local `MEMORY.md` after completed agent runs
- Workspace context-file auto-loading `AGENTS.md`, `CLAUDE.md`, `.kerux.md`, `KERUX.md`, prompt-injection scanning child-agent ReAct Kerux-supported provider-specific TUI context-token auto-compaction Prompt-prefixed TUI Claude/Anthropic-style SSE tool-use OpenAI-compatible streaming chunks
- Tree-sitter repo map AST-based file ranking personalized PageRank token-efficient codebase context (`[agent].repo_map_tokens`)
- Aider-style SEARCH/REPLACE blocks lean code generation (vs. full-file rewrites) via `edit_block` tool; model routing hints `EditFormat::SearchReplace/Patch` from capabilities table
- Repo-map file capping via `[agent].repo_map_max_files` (defaults to 500 files) prevents ranking stalls on very large repositories

- Git harness pre-run snapshots dirty-tree protection Conventional Commit derivation from staged diffs `commit_transaction`, `/undo` rollback command
- Lifecycle curator `kerux_core::curator` background pass memory importance decay near-duplicate pruning session auto-archiving stale skill archiving into `_archive/` tag-clustered distillation long-term facts draft skills; runs non-blockingly every agent startup and autonomous tick
- Memory pinning `pinned: true` flag `MemoryManager::set_pinned` MEMORY.md roundtrip curator exemption from decay/prune/dedup; pinned skills exempt archival
- Optional LLM-assisted skill summarization `skill_distill_llm_summary` + `curate_with_llm` rewrites distilled drafts as prose fails back bullet lists CLI runtime client passed when enabled
- Periodic mid-session passes `[curator].interval_secs` spawned once per process first tick delayed so startup/tick passes don't double up
- **Skill provenance metadata**: `SkillOrigin::Agent/User`, `Skill.pinned`, `use_count`, `last_activity_at` parsed/written SKILL.md front matter; curator auto-archive only touches agent-created unpinned skills; `SkillManager.record_use()` / `update_metadata()` persist fields atomically
