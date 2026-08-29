# Cron Scheduler

Recurring jobs that fire agent prompts on an interval.

## Commands

```
/cron add <interval> <prompt>                 — schedule an interval job
/cron add cron "<5-field-expr>" <prompt>       — schedule a 5-field cron job
/cron add once <timestamp> <prompt>           — schedule a one-shot job
/cron add agent <interval|cron|once> <task>   — schedule a full agent task run
/cron list                                    — show all jobs
/cron pause <id>                              — pause
/cron resume <id>                             — resume
/cron remove <id>                             — delete
```

## Schedule Syntax

- Intervals: `30m`, `2h`, `1d`, `1h30m` (minimum 60s)
- Cron: 5-field expression `minute hour day month weekday` (e.g. `*/5 * * * *`, `0 9 * * 1-5`)
- One-shot: ISO-8601 UTC timestamp (e.g. `2026-08-28T19:30:00Z`) or epoch seconds

## Behavior

- Jobs persist to `~/.kerux/scheduler.json` (atomic writes)
- Background ticker in `Gateway::run()` checks due jobs each tick
- Downtime burst protection: if multiple fires were missed while the process was down, only ONE catch-up run fires
- Job output is delivered to the channel that created it

## Implementation

- `kerux-core/src/scheduler.rs` — `Scheduler` with atomic JSON persistence
