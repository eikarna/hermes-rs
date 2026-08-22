# Kerux Code Audit Report (Task t_34d3f170)

Scope: all source under `crates/` (66 .rs files, ~42k lines), workspace `Cargo.toml`,
`kerux.example.toml`, `TODO.md`.

## Mechanical verification — ALL CLEAN

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace` | PASS |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | PASS (0 warnings) |
| `cargo test --workspace` | PASS — 534 tests (110 CLI + 424 core), 0 failed |

No syntax errors anywhere. Compiler and clippy are completely quiet at `-D warnings`.

> **Integration status (2026-08-22):** The table above records the audit baseline.
> Findings F1–F4 were subsequently resolved and integrated into the root task branch;
> see [Resolution status](#resolution-status) below. The integrated branch passes 544
> tests (110 CLI + 434 core), `cargo fmt --all -- --check`, workspace check, and clippy
> with `-D warnings`. F5 remains a non-blocking maintainability note.

## Findings

### F1 [MEDIUM — logic bug] Terminal tool timeout applies per-line, not per-command
`crates/kerux-core/src/tools/terminal_tool.rs:124-151`

The full `timeout` window is re-armed for **every** `reader.next_line().await`
(stdout loop :124, stderr loop :142) and again for `child.wait()` (:151).
A process that keeps stdout open and emits one line every `< max_timeout_secs`
never lets the loop exit, so the tool can run for **unbounded wall time** —
the configured cap never fires because `kill()` is only reached via the
`:151` wait timeout, which is never reached while the read loops spin.
Fix direction: capture `Instant::now()` once and compute `remaining()` for
each phase (or wrap the whole read+wait section in one deadline).

### F2 [TECH DEBT — fragile error-string protocol] HTTP status smuggled through `Error::Agent`
`crates/kerux-core/src/client.rs:219`, `:254` format `"HTTP {status}: {body}"`
into `Error::Agent`; `crates/kerux-core/src/client/fallback.rs:44-53` then
parses the status back out via `strip_prefix("HTTP ")`.
Works today and has dedicated regression tests (`fallback.rs:212-221`),
but any refactor that re-wraps or reformats the message silently disables
fallback on 429/5xx. Fix direction: structured variant
(`Error::Http { status, body }`) matched by enum instead of string prefix.

### F3 [LOW — perf/UI] Blocking subprocess probes inside TUI refresh
`crates/kerux-core/src/skills.rs:622-641` `command_exists()` shells out to
`which`/`where` per prerequisite command; called from
`crates/kerux-cli/src/tui/app.rs:623 refresh_skills()` which runs at startup
(:98) and on several UI actions (:334, :424, :750, :761, :788). Synchronous
`std::process::Command` on the UI thread. Impact small (fast binaries) but a
stalled PATH lookup freezes the TUI. Same pattern:
`githarness.rs` (blocking git) is invoked from async agent code at
`agent.rs:751` without `spawn_blocking` — a hung git (credential prompt,
stale NFS) stalls the tokio worker instead of just failing.

### F4 [INFO — silent poisoning swallow] Approval registry ignores mutex poisoning
`crates/kerux-core/src/gateway.rs:440`, `:459`
`PENDING_APPROVALS.lock().map(...).ok()` drops a poisoned-lock error, so if a
thread ever panicked while holding it, approval registration would silently
no-op afterwards (tool calls would proceed unprompted or hang depending on
gate wiring). Consider `unwrap_or_else(|p| p.into_inner())` or propagating.

### F5 [INFO — maintainability] File size hotspots
`agent.rs` 4483 lines, `run_journal.rs` 1926, `client.rs` 1788, `auth.rs`
1794. `agent.rs` mixes conversation state, recording, healing, tool loop and
edit-format logic — prime candidate for module split next time it is touched.

## Verified non-issues (checked, OK)

- **Production `unwrap()`s: only 2** outside `#[cfg(test)]`/benches, both provably safe:
  `screenshot.rs:55` (`file_stem` on hardcoded `"main.png"` — panics only if a future
  entry loses its extension), `scheduler.rs:211` (`to_digit` guarded by
  `is_ascii_digit()` on the previous line).
- All `panic!/unreachable!` sites are test-only except `provider.rs:465`, a legitimate
  internal invariant guard.
- `unsafe` blocks confined to `run_journal.rs:850-930` (flock / LockFileEx): correct
  SAFETY comments, proper `WouldBlock` / `ERROR_LOCK_VIOLATION` mapping, and the Windows
  side locks a sentinel byte at offset ~u64::MAX with a documented mandatory-lock
  rationale. Sound.
- Approval gate (`approval.rs`): trait contract mandates bounded waits; stale-button
  double-resolve is a no-op; dropped waiters error instead of hanging.
- Fallback chain policy correct: only network/429/5xx/incomplete-SSE fall over;
  auth and bad-request fail fast (no silent model downgrade).
- `kerux.example.toml` covers every section/field group of `AppConfig` (13/13 mapped);
  `TODO.md` ledger matches implemented feature set.
- Session store is file-based JSON per channel — no SQL injection surface.
- Workspace deps current (tokio 1.36, reqwest 0.12 rustls, clap 4); release profile tuned.

## Recommended follow-ups (not executed here — separate cards)

1. Fix F1 deadline handling in `terminal_tool.rs` (real correctness bug).
2. Introduce `Error::Http { status }` and match enum-wise in `fallback.rs` (F2).
3. Wrap `GitHarness` calls from async paths in `spawn_blocking`; consider caching
   `command_exists` results per session in the TUI (F3).

## Resolution status

| Finding | Status | Integrated resolution |
|---|---|---|
| F1 | Resolved | Terminal execution now uses one shared `Instant` deadline across stdout, stderr, and child wait/kill phases, with regression coverage for trickle-output processes. |
| F2 | Resolved | HTTP failures use the typed `Error::Http { status, body }` variant; fallback classification matches the enum directly. Gemini chat and streaming errors use the same typed path. |
| F3 | Resolved | Prerequisite and platform command probes scan `PATH` without spawning `which`/`where` or candidate binaries. Blocking `GitHarness` calls from async agent/TUI paths run via `tokio::task::spawn_blocking`. |
| F4 | Resolved | Poisoned synchronous mutexes are recovered with warnings; approval gates fail closed when no usable gate can be recovered. Regression tests cover approval bypass and pending-request preservation. |
| F5 | Open (informational) | File-size hotspots remain documented for opportunistic future module extraction; no correctness defect was identified. |
