# Agent Loop

The core execution engine lives in `kerux-core/src/agent.rs`.

## ReAct Cycle

1. **Build context** — system prompt + conversation history (+ `[CONTEXT SUMMARY]` if compaction active)
2. **Call LLM** — streaming or non-streaming via the configured provider
3. **Parse response** — text chunks, reasoning, tool calls (tolerant parsing)
4. **Execute tools** — with optional approval gate; results appended as tool messages
5. **Loop** — repeat until the model produces a final text response with no tool calls

## Cooperative Cancellation

Every iteration checks an `Arc<AtomicBool>` cancel flag — at loop boundaries, between SSE chunks, and before each tool execution. On cancel, `repair_conversation_after_cancel()` fixes any dangling assistant/tool message pairs so the conversation stays valid.

## Event Emission

The agent emits events (`ToolStart`, `ToolEnd`, `TextChunk`, `RunProgress`) via a bounded channel using non-blocking `try_send()` — a slow or dead event pump can never deadlock the ReAct loop.

## Context Compaction

When the conversation approaches the session cap, `compact_history()` summarizes the oldest messages via a one-shot LLM chat. The summary is stored in the session file (format v2) and injected as a `[CONTEXT SUMMARY]` marker, keeping recent messages intact.
