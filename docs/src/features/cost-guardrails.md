# Cost Guardrails

Estimated-spend ceilings enforced inside the agent loop. The guardrail is
off by default; enabling it applies per-run and/or daily cost ceilings on
top of the `[telemetry]` token prices.

## Configuration

```toml
[telemetry]
input_cost_per_million = 3.0   # token prices drive the cost estimates
output_cost_per_million = 15.0

[budget]
enabled = true
per_run_limit = 2.0            # estimated-spend ceiling per agent run (0 = off)
daily_limit = 20.0             # estimated-spend ceiling per rolling day (0 = off)
warn_threshold_pct = 80        # warn once at this % of a configured limit
on_limit = "downgrade"         # pause | downgrade | stop
downgrade_model = "gpt-4o-mini" # required when on_limit = "downgrade"
```

Invalid policies fail config load (`warn_threshold_pct > 100`, negative
limits, unknown `on_limit`, or `downgrade` without `downgrade_model`).

## Enforcement semantics

After every LLM response the agent records the turn's usage
(`record_tokens`) and evaluates the verdict:

- **Ok** — within all ceilings; nothing happens.
- **Warn** — estimated spend crossed `warn_threshold_pct` of a configured
  limit. The agent emits one `BudgetAlert` event (action `None`) per run;
  the run continues.
- **LimitExceeded** — a hard ceiling was crossed. The configured
  `on_limit` action applies:
  - `pause` / `stop` — the agent emits a `BudgetAlert` with the action and
    halts the run with `Error::BudgetExceeded`.
  - `downgrade` — the agent emits one `BudgetAlert` and routes the rest of
    the run to `downgrade_model`. The downgrade applies once; the run
    continues on the cheaper model.

Costs are estimates computed from the `[telemetry]` rates and the
provider-reported usage (token estimates when a provider reports no
usage).

## Surfaces

- **Agent events** — `AgentEvent::BudgetAlert { action, reason,
  current_run_cost, daily_cost, downgrade_model }` is journaled as a
  `budget_alert` record when the flight recorder is active.
- **Telemetry** — with the guardrail enabled, each billable
  `AgentTelemetry` carries `estimated_cost_usd` for the turn (provider
  quotes still win when present).
- **Gateway** — budget alerts render as a `⚠️ Budget: <reason>` status
  message in the chat channel.
- **TUI** — budget alerts appear in the Activity panel with run/day cost
  and the downgrade target.

## Run and daily accounting

Per-run state (run cost, warn/downgrade once-flags, model override) resets
at the start of every `run()`. The daily accumulator survives across runs
on shared agents (gateway, TUI). Autonomous mode builds a fresh agent per
tick but seeds it with the shared daily accumulator and snapshots the
updated totals back afterwards, so the daily ceiling spans ticks.

## Testing

- Unit tests in `crates/kerux-core/src/agent.rs` cover each verdict path
  (warn once, pause halt, stop halt, downgrade once + reset, disabled
  no-op).
- `crates/kerux-core/tests/cost_guardrail_wiring.rs` is the end-to-end
  wiring test: a scripted provider run crosses the per-run limit, the
  remaining turn routes to the downgrade model, one downgrade alert is
  emitted, and billable telemetry carries the per-turn cost.
