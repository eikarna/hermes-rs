# Subagent Delegation (F6)

The agent can delegate focused tasks to isolated child agents.

## How It Works

`SubAgentTool` registers a `delegate_to_sub_agent` tool. When invoked:

1. A child `KeruxAgent` is spawned with a **fresh conversation** (no parent history)
2. The child runs its own ReAct loop and returns a final summary
3. Only the summary enters the parent conversation — intermediate noise stays out

## Guardrails

| Guardrail | Value |
|---|---|
| Default | **ON** |
| Max concurrent children | 3 (tokio semaphore) |
| Nesting depth | 1 (child registry is empty — children cannot delegate further) |

## Config

```toml
[delegation]
provider = "openai"        # optional: separate provider for children
model = "gpt-4o-mini"      # optional: cheaper model for delegation
max_concurrent = 3
```

## Implementation

- `kerux-core/src/tools/sub_agent_tool.rs`
