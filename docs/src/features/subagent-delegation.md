# Subagent Delegation

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
[tools.delegation]
enabled = true           # default: the tool only costs tokens when called
max_concurrent = 3       # shared semaphore across the process
```

Children share the parent's configured provider and model — there is no separate delegation provider setting.

## Implementation

- `kerux-core/src/tools/sub_agent_tool.rs`
