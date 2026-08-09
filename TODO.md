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

- Model-agnostic provider routing with an internal `LLMProvider` trait, provider capability metadata, `[client].provider` selection, and native adapters for OpenAI, Anthropic, Ollama, and OpenRouter
- Tree-sitter AST symbol extraction (C, Python, Rust, TypeScript) with personalized PageRank repository mapping and token-budgeted `<repo_map>` rendering
- Aider-style SEARCH/REPLACE edit block parser (`parse_edit_blocks`), atomic multi-edit `edit_block` tool with exact + fuzzy matching, and capability-driven routing that injects an `<edit_format>` hint when the provider advertises `EditFormat::SearchReplace`
- Per-model capability tables with longest-prefix matching (`lookup_capabilities`) covering Claude, GPT, and o-series models, richer metadata (`supports_vision`, `supports_tool_calls`), and a `patch`-tool hint for models advertising `EditFormat::Patch`

## Pending

- Wire repo map into agent context injection (currently exposed for consumers but not auto-attached to sessions)
- [Aider Phase 4] Implement transactional git harness (dirty tree protection, Conventional Commit generation, `/undo` command)
- [Aider Phase 5] Port skill & memory lifecycle management (curator background task, skill distillation, auto-archiving)
