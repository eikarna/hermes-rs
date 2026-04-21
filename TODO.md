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

## Pending

- Run end-to-end autonomous mode validation against a disposable sample repository and document the operator workflow
- Add richer autonomous observability so repeated failures and paused states are summarized in a dedicated status report
- Consider persisting autonomous failure state across process restarts if long-running deployments need resume behavior
