# Hermes-RS TODO

## Implemented

- ReAct agent orchestration loop through `HermesAgent::run()`
- Shared TOML runtime configuration across `hermes-core` and `hermes-cli`
- Ratatui prompt-first TUI with conversation, reasoning, activity, MCP, skills, and behavior panels
- Streaming and non-streaming LLM request handling with tolerant reasoning/tool-call parsing
- Built-in file, patch, terminal, code execution, web, memory, and TODO tools
- GitHub Actions build, test, coverage, and release workflows with changelog-driven release notes
- Autonomous coding mode entrypoints: `hermes autonomous` and `hermes run --autonomous`
- Autonomous workspace loop that reads `TODO.md`, runs the agent, validates changes, and only pushes after passing tests
- End-to-end autonomous mode validation against a disposable sample repository, with README operator workflow documentation
- Dedicated repo-local `autonomous-status.toml` reporting for autonomous state, validation summaries, repeated failures, and paused states
- Persistent autonomous failure pause state across process restarts until `TODO.md` or git state changes
- State distillation with long-term memory injection and async session fact extraction into `MEMORY.md`
- Workspace context-file auto-loading with prompt-injection scanning for agent guidance files
- Sub-agent delegation as an opt-in built-in tool through `delegate_to_sub_agent`

- Model-agnostic provider routing with an internal `LLMProvider` trait, provider capability metadata, `[client].provider` selection, and native adapters for OpenAI, Anthropic, Ollama, OpenRouter, and Gemini
- Tree-sitter AST symbol extraction (C, Python, Rust, TypeScript) with personalized PageRank repository mapping and token-budgeted `<repo_map>` rendering
- Aider-style SEARCH/REPLACE edit block parser (`parse_edit_blocks`), atomic multi-edit `edit_block` tool with exact + fuzzy matching, and capability-driven routing that injects an `<edit_format>` hint when the provider advertises `EditFormat::SearchReplace`
- Per-model capability tables with longest-prefix matching (`lookup_capabilities`) covering Claude, GPT, and o-series models, richer metadata (`supports_vision`, `supports_tool_calls`), and a `patch`-tool hint for models advertising `EditFormat::Patch`
- Optional `[agent].edit_format_override` forces the `<edit_format>` hint (`search_replace`/`patch`/`full_file`) when capability-table prefix rows guess wrong
- Repo-map context injection: `[agent] repo_map_tokens` budget renders a `<repo_map>` block into the system prompt (parsed once per agent, off the async worker)
- Transactional git harness (`hermes_core::githarness`): pre-run snapshots with dirty-tree protection, Conventional Commit message derivation from staged diffs, `commit_transaction`, `undo`, a TUI `/undo` command that rolls back the last run's file changes, and optional post-run auto-commit via `[agent].auto_commit`
- Skill & memory lifecycle management: background curator pass (`hermes_core::curator`) with memory importance decay and near-duplicate pruning, session auto-archiving, stale skill archiving into `_archive/`, and tag-clustered distillation of long-term facts into draft skills; runs non-blockingly on every agent startup and autonomous tick
- Memory pinning (`pinned` flag) that survives MEMORY.md roundtrip and exempts blocks from curator decay/prune/dedup, with `MemoryManager::set_pinned` persisting outside the write lock
- Optional LLM-assisted skill summarization (`skill_distill_llm_summary` + `curate_with_llm`) rewriting distilled draft skills as prose, and periodic mid-session curator passes via `[curator].interval_secs`
- Skill approval flow: `[curator].auto_approve_skills = false` (default) routes distilled drafts to `<skills>/_pending/` where they stay unloadable until approved (`a` in the TUI Skills panel; `d` discards pending)
- Trajectory compression: `[curator].compression_min_age_days` / `compression_max_importance` / `compression_min_count` fold old, low-importance, unpinned facts into one deterministic `session_summary` block per curator pass (no LLM; distilled/pinned facts exempt)

## Pending

- [Release] Bump to `0.2.0` and tag for the next binary release once Phases 4–5 soak in (repo map + edit blocks + git harness + curator)

