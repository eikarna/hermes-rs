# Cron Scheduler (F5)

Recurring jobs that fire agent prompts on an interval.

## Commands

```
/cron add <name> <interval> <prompt>   — schedule a job
/cron list                             — show all jobs
/cron pause <name>                     — pause
/cron resume <name>                    — resume
/cron remove <name>                    — delete
```

## Interval Syntax

Stdlib-parsed durations: `30m`, `2h`, `1d`, `1h30m`.

## Behavior

- Jobs persist to `~/.kerux/cron/jobs.json` (atomic writes)
- Background ticker in `Gateway::run()` checks due jobs each tick
- Downtime burst protection: if multiple fires were missed while the process was down, only ONE catch-up run fires
- Job output is delivered to the channel that created it

## Implementation

- `kerux-core/src/scheduler.rs` — `Scheduler` with atomic JSON persistence
