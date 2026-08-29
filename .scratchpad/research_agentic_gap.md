# Kerux: Internal Codebase State & Current Advantage

## Internal Subsystems
1. `scheduler.rs` / `agent.rs`: Orchestration of context window, API calls, tool dispatching.
2. `memory.rs` / `taste.rs` / `taste_extraction.rs`: Taste profiles, hierarchical memory, context grounding.
3. `tools.rs` / `mcp.rs`: Model Context Protocol integration, local commands, browser tools.
4. `autonomous.rs` / `tui/`: TUI, event loop, and headless orchestration for self-healing loops.
5. `guardrails.rs` (likely folded into `validators.rs` / `validation.rs`): Guardrails, output parsing, schema enforcement.
6. `trajectory.rs`: Trajectory saving for RL training data generation (RLHF, RLAIF).

## The Kerux Uniqueness Factor (Currently)
1. Native Rust footprint -> Very fast parsing, lower memory overhead vs Python/JS agents.
2. `taste.rs` subsystem -> Evaluates the structural preferences of a developer and dynamically adapts output style.
3. Ratatui-based Rich TUI -> Interactive visual breakdown of conversation, MCP state, skills, reasoning.
4. Autonomous Mode -> Background self-healing.
5. RLHF Trajectory generation -> Pre-built pipeline to emit training data from real interactions.

---

# External Ecosystem Analysis: Gap & Complaints

## 1. CLI / Terminal Agents (Claude Code, OpenCode, Aider, Codex)
**Primary Complaints:**
- Context Gaps & Token Limits: Losing context mid-flow, forgetting what was done early in the session, or struggling with multi-file refactors.
- Refactoring Failures: Cursor and CLI tools alike struggle when changes span dozens of interdependent modules. They optimize for single-file or localized edits.
- Environment Sync: Issues persisting "memories" and configurations across sessions (e.g., Anthropic Claude Code issues with harness stability over time).

## 2. Autonomous Frameworks (Hermes, Devin, SWE-Agent)
**Primary Complaints:**
- Devin's Local vs Cloud Gap: Devin Local cannot use cloud workflows, cannot persist memories between sessions natively (requires migrating to "skills"), and lacks conversation sharing and App Deploys.
- Resource Intensive / Degradation: Devin consumes "Agent Compute Units" (ACU) heavily when confused (e.g., looping on a PR fix) and its performance noticeably degrades after a long conversation.
- Hand-holding: Struggles without extreme context scaffolding. It gets stuck on CI build errors and attempts unrelated fixes (e.g., refactoring auth when a database connection fails).

## 3. IDE Agents (Cursor, Windsurf, Copilot Workspace)
**Primary Complaints:**
- Multi-file Context Loss: Cursor fails badly in enterprise-scale multi-file refactors. It loses track of how changes propagate.
- Editor Lock-in: Cursor and Windsurf (Devin Desktop) require custom VS Code forks, alienating JetBrains or Neovim power users.

## 4. Orchestration & Workflows (n8n, LangGraph, CrewAI)
**Primary Complaints:**
- Complexity Overhead (LangGraph): Adding a simple intent requires refactoring the entire state schema across nodes and conditional edges.
- Static Workflows (CrewAI): Great at defined roles, terrible at dynamic branching or concurrent ad-hoc tasks.
- Ecosystem Lock-in: Hard to swap out LLM components or integrate native binaries without building Python wrappers.
