# Kerux Lab: Flight Recorder to Causal Debugger Implementation Plan

> **For Hermes:** Use `subagent-driven-development` to implement this plan task-by-task. Do not skip the RED → GREEN → REFACTOR cycle or the staged-file review before each commit.

**Goal:** Build Kerux's signature evidence pipeline incrementally: truthfully bounded execution, a redacted append-only Flight Recorder, inspectable/verifiable Proof Capsules, evidence-driven validation, isolated model tournaments, counterfactual replay, transactional multi-agent integration, and regression-gated self-improvement.

**Architecture:** Add a versioned `run_journal` domain beside the existing RL-oriented `trajectory` module rather than repurposing it. Every native Kerux run produces a redacted, hash-chained NDJSON event stream plus a small manifest under `KERUX_HOME/runs/`; CLI readers verify and inspect it without executing recorded commands. Later phases consume this canonical journal to build validation, worktree arenas, counterfactual forks, skill regression gates, and budget optimization.

**Tech Stack:** Rust 2021, Tokio, Serde/Serde JSON, SHA-256 (`sha2`, already present), regex (already present), Clap, existing `GitHarness`, existing `AgentEvent`/telemetry, and static HTML generated with the standard library. No new dependency is allowed without Nix's explicit approval.

---

## 1. Scope and implementation contract

### In scope

1. **Phase 0 — Safety truthfulness**
   - Stop calling host child-process execution a secure sandbox.
   - Add bounded recording and deterministic redaction before any journal write.
   - Extend approval evidence without pretending approval equals sandboxing.
2. **Phase 1 — Kerux Black Box / Flight Recorder**
   - Append-only event journal, integrity verification, Git references, run inspection, and Proof Capsule export.
3. **Phase 2 — Evidence Engine**
   - Configured format/lint/test validators, first-pass/repair-pass outcomes, adaptive edit-protocol telemetry.
4. **Phase 3 — Multiverse Arena**
   - Native Kerux contestants in isolated Git worktrees, deterministic scoring, explicit winner selection.
5. **Phase 4 — Causal Time Machine**
   - Fork a run at a checkpoint, change one declared variable, replay, compare, and perform bounded context ablation/bisection.
6. **Phase 5 — Transactional Swarm**
   - Prepare, verify, reconcile, atomic integrate, and rollback a dependency graph of worktree changes.
7. **Phase 6 — Skill Darwinism and Outcome Budget Autopilot**
   - Regression-gate candidate skills and route work according to measured verified-success-per-cost.

### Explicit non-goals

- No blockchain, remote attestation, public cloud service, marketplace, visual node editor, or social feed.
- No claim of deterministic LLM output. “Replay” means reproducible input/state reconstruction plus explicit divergence detection.
- No raw chain-of-thought persistence. Reasoning events store metadata/digests by default, not private reasoning text.
- No generic container orchestrator.
- No support for Claude Code/Aider/OpenCode adapters in the first Arena release.
- No automatic merge to the user's main branch.
- No cryptographic **signature** claim in v1. SHA-256 chaining is tamper-evidence, not identity attestation.
- No OS sandbox dependency until a separate decision gate is approved.

### User-visible terminology

- **Flight Recorder:** local canonical event journal.
- **Run:** one agent request from start to terminal outcome.
- **Proof Capsule:** portable, redacted export of a run and its evidence.
- **Replay:** reconstruct a prior run's inputs and state; external side effects are never silently repeated.
- **Fork:** new run derived from a prior checkpoint with a declared intervention.
- **Causal finding:** an experimentally reproduced outcome difference, never a claim inferred from one model explanation alone.

---

## 2. Current-state findings that constrain the design

1. `crates/kerux-core/src/trajectory.rs` is an RL/training export model and is only exercised by its own tests. It is not the runtime event source. Keep it backward compatible and derive training trajectories from journals later.
2. `crates/kerux-core/src/agent.rs` already emits `AgentEvent::{Thinking,Reasoning,ToolStart,ToolComplete,ToolError,Content,Done,IterationComplete,Telemetry,Error}` through one swappable MPSC sender.
3. Gateway consumers currently own that event sender. The recorder must not steal it or require every surface to duplicate recording logic.
4. `GitHarness` can snapshot/undo, but `commit_transaction()` stages with `git add -A`. Do not use that method for implementation commits; use explicit human-controlled staged file lists.
5. `code_execution.rs` currently launches host Python/Node/shell/Rust processes. Timeout is not filesystem/network/environment isolation.
6. `delegate_to_sub_agent` currently uses the parent model and an empty tool registry. It is a reasoning-only child, not yet a coding worker.
7. `sha2`, `regex`, `serde_json`, `tokio`, `getrandom`, and test-only `tempfile` already exist. Flight Recorder MVP requires no new crate.
8. Current public release is `0.2.0`, but `AGENTS.md` still says `0.1.3`; correct this in the first documentation commit.

---

## 3. Architectural decisions

### ADR-1: Separate journal from trajectory

Create `crates/kerux-core/src/run_journal.rs`. Do not mutate the meaning of `Trajectory`, whose fields are optimized for RL export rather than crash-safe provenance.

Later, add a pure converter:

```rust
pub fn trajectory_from_run(run: &VerifiedRun) -> Trajectory;
```

Only add that converter when a real consumer needs it.

### ADR-2: One canonical journal, many read-only projections

Storage layout:

```text
$KERUX_HOME/runs/
└── <run_id>/
    ├── manifest.json
    ├── events.ndjson
    └── artifacts/
```

- `events.ndjson` is append-only during a run.
- `manifest.json` is written atomically at run start and replaced atomically at completion.
- Artifact bodies are opt-in and content-addressed; v1 does not persist arbitrary workspace files.
- CLI/TUI/HTML views are projections; they never become canonical state.

### ADR-3: Hash-chain semantics

Each persisted line is a `RunEventEnvelope`:

```rust
pub struct RunEventEnvelope {
    pub schema_version: u32,
    pub run_id: String,
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub kind: RunEventKind,
    pub payload: serde_json::Value,
    pub previous_hash: Option<String>,
    pub hash: String,
}
```

Hash input is the compact JSON serialization of every field except `hash`, in struct field order. Verification must reject sequence gaps, wrong `previous_hash`, wrong content hash, mismatched run IDs, and invalid final status.

A truncated final NDJSON line is reported as `IncompleteTail`, not silently accepted and not treated as evidence of tampering.

### ADR-4: Central recording without breaking UI event delivery

Do not multiplex the existing MPSC channel at surface level. Add an optional recorder handle to `KeruxAgent`:

```rust
run_recorder: Arc<std::sync::Mutex<Option<Arc<RunRecorder>>>>
```

Expose `set_run_recorder(...)`, parallel to `set_event_sender(...)`. `emit()` keeps sending `AgentEvent` to UI/gateway and also maps recordable data to a sanitized `RunEventKind`.

Events unavailable through `AgentEvent`—run start, prepared request manifest, approval request/decision, cancellation, and Git metadata—must be recorded at the authoritative call site.

### ADR-5: Redaction happens before persistence

No API may accept “raw now, redact during export.” The writer receives a `RedactedValue` or invokes the redactor before serialization. Defaults:

- Exact sensitive JSON keys: `authorization`, `api_key`, `apikey`, `token`, `access_token`, `refresh_token`, `password`, `secret`, `cookie`, `set-cookie`.
- Known bearer/API-key patterns in strings.
- Values matching configured secret environment variables, loaded only into an in-memory matcher.
- Raw reasoning text omitted; persist character count and SHA-256 digest.
- Tool arguments/results redacted and size-bounded before hashing/writing.
- Oversized payloads store prefix/suffix plus original byte count and digest.

Redaction failures are fail-closed: record a placeholder and warning event, never raw data.

### ADR-6: Local journal is not automatically shareable

Local journals may contain redacted prompts and paths. Capsule export performs a second export-redaction pass:

- Replace absolute home/workspace prefixes.
- Omit channel/user identifiers unless explicitly included.
- Omit raw environment and auth profile names.
- Include a machine-readable redaction report.

### ADR-7: Validation before judging

Arena winner ordering:

1. Required validator pass/fail.
2. Security/policy violations.
3. User-declared invariants.
4. Test/lint/format results.
5. Diff risk/size and runtime.
6. Cost.
7. Optional model judge only for unresolved qualitative ties.

A model judge can never override a failed deterministic required validator.

### ADR-8: No “secure sandbox” claim until verified

Phase 0 corrects terminology to “bounded host execution.” A true sandbox requires a separate RFC covering Linux/macOS/Windows behavior. Until approved and implemented, Arena contestants run only in explicit user-approved worktrees with policy and environment scrubbing; this is isolation of repository changes, not OS isolation.

---

## 4. Versioned journal schema v1

### `RunManifestV1`

Required fields:

```text
schema_version
run_id
parent_run_id?          # set only for forked runs
parent_sequence?        # fork point
created_at_ms
completed_at_ms?
status                  # running|succeeded|failed|cancelled|incomplete
surface                 # cli|tui|gateway|autonomous
model
provider_kind
workspace_fingerprint   # hash, not raw path in capsules
repository_head?
repository_dirty_hash?
recorder_policy
last_sequence
last_hash?
replayability           # full|degraded|inspection_only
warnings[]
```

### `RunEventKindV1`

Initial variants only:

```text
run_started
request_prepared
thinking_metadata
reasoning_metadata
content_delta
assistant_message
approval_requested
approval_decided
tool_started
tool_completed
tool_failed
telemetry
iteration_completed
git_checkpoint
validation_started
validation_completed
run_cancelled
run_failed
run_completed
redaction_warning
```

Do not add speculative variants for Arena/Swarm in schema v1. Future readers must ignore unknown variants while preserving their raw payload.

### Replayability rules

- `full`: prompt/context and required deterministic tool observations are present after redaction; secrets may be re-injected at execution time.
- `degraded`: at least one required payload was omitted/truncated/redacted in a way that may change behavior.
- `inspection_only`: no safe execution reconstruction is possible.

Readers must display replayability prominently.

---

## 5. Git staged-commit protocol

Every implementation unit follows this exact sequence:

```bash
# 1. Start from a clean tree
git status --short

# 2. Write one failing test and prove RED
cargo test -p kerux-core <exact_test_name> -- --exact

# 3. Implement only that behavior and prove GREEN
cargo test -p kerux-core <exact_test_name> -- --exact

# 4. Run affected crate tests and formatting
cargo fmt --all -- --check
cargo test -p kerux-core

# 5. Stage exact files only
git add path/to/tested.rs path/to/related-doc.md

# 6. Review staged content
git diff --cached --check
git diff --cached --stat
git diff --cached

# 7. Commit only after review
git commit -m "type(scope): one behavior"

# 8. Confirm no accidental staged files
git status --short
```

Rules:

- Never use `git add -A`, `git add .`, or `GitHarness::commit_transaction()` for this implementation series.
- Tests and the minimal implementation they specify belong in the same commit.
- Public docs/config examples ship in the same commit as user-visible behavior.
- Pure architectural docs may have their own `docs:` commit.
- Do not amend or squash earlier approved commits unless Nix explicitly requests it.
- Tag each completed phase in the plan checklist with its commit hash, but do not store volatile hashes in persistent memory.
- Full phase gate:

```bash
cargo fmt --all
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

---

# 6. Phase 0 — Safety truthfulness and recorder prerequisites

## Task 0.1: Correct execution claims and stale release metadata

**Objective:** Ensure source/docs accurately state that code executes as bounded host child processes, not inside an OS sandbox.

**Files:**
- Modify: `crates/kerux-core/src/tools/code_execution.rs:1-4,39-41`
- Modify: `AGENTS.md:5`
- Search/modify only if claims exist: `README.md`, `docs/src/**/*.md`

**Steps:**
1. Add a documentation test/search assertion or CI-friendly repository search for prohibited phrase `secure code execution in a sandboxed environment`.
2. Confirm it fails against current source.
3. Replace the claim with precise bounded-host wording and security limitations.
4. Update release line `0.1.3` → `0.2.x`.
5. Run affected tests and repository search.
6. Stage exact documentation/source files.

**Acceptance:** No public/source documentation calls child-process execution a sandbox; behavior is unchanged.

**Commit:** `docs(security): clarify host code execution boundaries`

## Task 0.2: Add pure recursive redaction primitives

**Objective:** Redact sensitive keys and known string patterns deterministically before persistence.

**Files:**
- Create: `crates/kerux-core/src/redaction.rs`
- Modify: `crates/kerux-core/src/lib.rs`

**TDD cases:**
- Nested object/array sensitive keys become `[REDACTED]`.
- Header keys are case-insensitive.
- Bearer values and recognized API-key prefixes inside command strings are removed.
- Benign words such as `token_budget` and ordinary source code are not over-redacted.
- Redaction is idempotent.
- Input `serde_json::Value` is not mutated unexpectedly.

**Acceptance:** Pure API has no filesystem access and all secret fixtures disappear from serialized output.

**Commit:** `feat(security): add deterministic journal redaction`

## Task 0.3: Add bounded payload representation

**Objective:** Prevent unbounded journal growth while preserving evidence metadata.

**Files:**
- Modify: `crates/kerux-core/src/redaction.rs`

**TDD cases:**
- Payload below limit is preserved after redaction.
- Oversized payload stores bounded prefix/suffix, original byte length, SHA-256, and `truncated=true`.
- UTF-8 boundaries remain valid.
- Secret spanning the truncation boundary cannot leak.

**Acceptance:** Serialized bounded payload never exceeds configured envelope overhead plus limit.

**Commit:** `feat(security): bound recorded payloads`

## Decision Gate 0A: Real OS sandbox

Before implementing OS isolation, present Nix with a separate RFC comparing:

- Linux: Landlock/seccomp/bubblewrap/container option.
- macOS: `sandbox-exec` limitations or container/VM option.
- Windows: Job Objects/restricted token/AppContainer limitations.
- Network deny/allow semantics.
- New dependency and maintenance costs.

**Default if not approved:** continue with truthful “bounded host execution,” approval policy, worktree isolation, environment scrubbing, and explicit warnings. Do not invent a partial sandbox abstraction.

---

# 7. Phase 1 — Flight Recorder vertical slice

## Task 1.1: Define schema and pure hash-chain functions

**Objective:** Establish stable journal v1 types and deterministic integrity verification.

**Files:**
- Create: `crates/kerux-core/src/run_journal.rs`
- Modify: `crates/kerux-core/src/lib.rs`

**TDD cases:**
- First event has sequence `0` and no previous hash.
- Second event links to first hash.
- Payload mutation fails verification.
- Sequence gap fails verification.
- Cross-run event fails verification.
- Unknown event kind remains readable as raw JSON.

**Acceptance:** Pure in-memory chain roundtrip verifies without filesystem access.

**Commit:** `feat(recorder): define versioned hash-chained run events`

## Task 1.2: Implement crash-aware append-only storage

**Objective:** Persist manifest atomically and events append-only under `KERUX_HOME/runs/<run_id>`.

**Files:**
- Modify: `crates/kerux-core/src/run_journal.rs`
- Reuse: `crates/kerux-core/src/persist.rs`

**TDD cases using `tempfile`:**
- `KERUX_HOME` override isolates tests.
- New run creates manifest and empty/event file with secure best-effort permissions.
- Append followed by reopen preserves next sequence/hash.
- Truncated final line yields `IncompleteTail` and keeps preceding verified events.
- Corruption in a completed line fails verification.
- Finalization atomically updates terminal status.

**Acceptance:** A crash cannot turn earlier verified events into silently accepted corrupt state.

**Commit:** `feat(recorder): persist crash-aware run journals`

## Task 1.3: Record agent lifecycle centrally

**Objective:** Wire the optional recorder into `KeruxAgent` without altering existing UI/gateway event delivery.

**Files:**
- Modify: `crates/kerux-core/src/agent.rs`
- Modify: `crates/kerux-core/src/run_journal.rs`

**TDD cases:**
- Existing event channel still receives events when recorder is attached.
- Tool start and matching completion share call ID.
- Cancellation and error produce terminal events.
- Exactly one terminal status is written.
- Recorder failure is surfaced as a warning/error according to configured fail mode; raw payload is never used as fallback.
- Reasoning text is omitted by default and replaced with metadata/digest.

**Acceptance:** CLI, TUI, gateway, and autonomous mode can use the same central recorder path.

**Commit:** `feat(agent): record redacted native run lifecycle`

## Task 1.4: Record prepared-request provenance

**Objective:** Capture what context classes influenced the request without persisting secrets or raw chain-of-thought.

**Files:**
- Modify: `crates/kerux-core/src/agent.rs`
- Modify: `crates/kerux-core/src/run_journal.rs`
- Potentially modify: `crates/kerux-core/src/context_files.rs`, `crates/kerux-core/src/skills.rs`, `crates/kerux-core/src/memory.rs` only if identifiers cannot be obtained centrally.

**Record:**
- Message role/content digest and redacted content according to policy.
- Loaded context-file paths relative to workspace plus content digests.
- Skill names/versions/digests.
- Memory block IDs/digests, not hidden secrets.
- Tool schema names/digests.
- Model/provider capability metadata.

**Acceptance:** Inspector can answer “what influenced this request?” without reading live mutable files.

**Commit:** `feat(recorder): capture prepared-request provenance`

## Task 1.5: Record approval decisions and execution boundaries

**Objective:** Make every approval request and outcome auditable.

**Files:**
- Modify: `crates/kerux-core/src/approval.rs`
- Modify: `crates/kerux-core/src/agent.rs`
- Modify: `crates/kerux-core/src/gateway.rs`
- Modify tests adjacent to approval handling.

**TDD cases:** approved, denied with redacted reason, timeout/auto-deny, stale response, and cancellation while waiting.

**Acceptance:** Tool execution can be correlated with its preceding approval decision; approval is never labelled sandbox enforcement.

**Commit:** `feat(recorder): journal tool approval decisions`

## Task 1.6: Attach Git checkpoint metadata

**Objective:** Record reproducible repository identity without persisting arbitrary dirty patches.

**Files:**
- Modify: `crates/kerux-core/src/githarness.rs`
- Modify: `crates/kerux-core/src/agent.rs` or the run-launch orchestration point selected during implementation.
- Modify: `crates/kerux-core/src/run_journal.rs`

**Record:** repository HEAD, branch, clean/dirty status, dirty patch SHA-256, and relative changed-file list. Raw dirty patch remains outside journal v1.

**Acceptance:** Run manifest clearly reports whether exact code-state reconstruction is possible.

**Commit:** `feat(recorder): attach git checkpoint evidence`

## Task 1.7: Add recorder configuration

**Objective:** Configure recorder behavior without speculative options.

**Files:**
- Modify: `crates/kerux-core/src/config.rs`
- Modify: `kerux.example.toml`
- Modify config tests.

**Minimal config:**

```toml
[recorder]
enabled = true
max_payload_bytes = 65536
record_content = true
record_reasoning = false
failure_mode = "warn" # warn|fail
```

No retention policy, remote upload, signing key, or compression in v1.

**Acceptance:** Existing configs retain compatible defaults; example config documents privacy implications.

**Commit:** `feat(config): expose bounded recorder policy`

## Task 1.8: Add `kerux runs list|inspect|verify`

**Objective:** Make journals useful before building any graphical UI.

**Files:**
- Modify: `crates/kerux-cli/src/main.rs`
- Create: `crates/kerux-cli/src/runs.rs`
- Add CLI parse and fixture-driven tests.

**Commands:**

```bash
kerux runs list [--json]
kerux runs inspect <run-id> [--json]
kerux runs verify <run-id> [--json]
```

Readers are strictly non-executing.

**Acceptance:** Corrupt fixtures return non-zero with a stable reason code; JSON output is machine-readable and contains no ANSI.

**Commit:** `feat(cli): inspect and verify recorded runs`

## Task 1.9: Export Proof Capsule v1

**Objective:** Export a portable directory/`.kerux` archive representation without adding an archive dependency.

**Files:**
- Create: `crates/kerux-core/src/capsule.rs`
- Modify: `crates/kerux-core/src/lib.rs`
- Modify: `crates/kerux-cli/src/runs.rs`

**MVP representation:** deterministic directory or single JSON bundle first. Do not invent ZIP support without an approved dependency.

**Command:**

```bash
kerux runs export <run-id> --output <path> [--html]
```

Static HTML must contain escaped data, embedded CSS/JS only, no CDN, no network fetch, and a clear integrity/replayability badge.

**Acceptance:** Exported HTML opens offline; fixture scan finds no seeded secret or absolute home path; capsule verifier passes.

**Commit:** `feat(capsule): export offline verifiable run evidence`

## Task 1.10: Document and demonstrate Flight Recorder

**Objective:** Publish accurate usage, threat boundaries, schema stability, and a reproducible demo.

**Files:**
- Create: `docs/src/features/flight-recorder.md`
- Modify: `docs/src/SUMMARY.md`
- Modify: `docs/src/development/roadmap.md`
- Modify: `README.md` only with a short link/positioning line.
- Modify: `CHANGELOG.md` when preparing the release, not earlier.

**Acceptance:** Docs explicitly distinguish hash-chain vs signature, replay vs deterministic reproduction, worktree vs sandbox, and local journal vs shareable capsule.

**Commit:** `docs(recorder): document Flight Recorder and Proof Capsules`

### Phase 1 exit gate

A fixture run must prove this end-to-end:

```text
run native Kerux request
→ journal events append
→ inspect timeline
→ verify hash chain
→ export redacted offline HTML
→ no seeded secret/path leakage
```

Do not start Arena until this passes in CI.

---

# 8. Phase 2 — Evidence Engine

## Task 2.1: Define `ValidationPolicy` and results

**Files:**
- Create: `crates/kerux-core/src/validation.rs`
- Modify: `crates/kerux-core/src/config.rs`
- Modify: `kerux.example.toml`
- Modify: `crates/kerux-core/src/lib.rs`

Minimal policy: ordered commands, required flag, timeout, working directory relative to workspace, and output cap. No plugin protocol yet.

**Commit:** `feat(validation): define deterministic project validators`

## Task 2.2: Execute validators with evidence events

Record command digest, start/end time, exit code, bounded/redacted output, and required/optional status. Never run validators from an imported capsule without explicit user action.

**Commit:** `feat(validation): record bounded validator evidence`

## Task 2.3: Separate first-pass and repair-pass outcomes

Add run attempt identity and a bounded repair policy. Record whether success came from first generation or evidence-fed repair.

**Commit:** `feat(agent): track first-pass and repair outcomes`

## Task 2.4: Measure edit-protocol outcomes

Record selected `EditFormat`, parse/apply status, model/provider, language, and repair count. Do not alter routing yet.

**Commit:** `feat(editing): measure edit protocol outcomes`

## Task 2.5: Add conservative adaptive fallback

Fallback only after a classified edit-application failure; never on semantic test failure. Start with a static, tested order derived from capabilities. Learned routing waits for sufficient local samples.

**Commit:** `feat(editing): fall back across compatible edit formats`

### Phase 2 exit gate

Two models solving the same fixture produce comparable first-pass, repair-pass, validator, cost/token, and edit-format evidence in the journal.

---

# 9. Phase 3 — Native Multiverse Arena

## Decision Gate 3A: Worktree lifecycle design

Before code, approve naming, cleanup, dirty-tree behavior, disk limits, cancellation, and whether failed worktrees are retained for inspection.

## Task sequence

1. **Arena manifest and contestant specification**
   Files: new `crates/kerux-core/src/arena.rs`; config only if a real setting is required.
   Commit: `feat(arena): define native contestant runs`
2. **Explicit worktree manager**
   Extend `githarness.rs` with create/list/remove methods that never delete unknown worktrees.
   Commit: `feat(git): isolate arena contestants in worktrees`
3. **Run two native contestants concurrently**
   Each gets its own model, journal, worktree, budget, and cancellation token.
   Commit: `feat(arena): run isolated native contestants`
4. **Deterministic scorecard**
   Required validator pass dominates; include policy violations, first-pass, runtime, tokens/cost when known, and diff size.
   Commit: `feat(arena): rank contestants by verified evidence`
5. **CLI**
   `kerux arena <task> --model <m1> --model <m2> --validate <policy>` with JSON output.
   Commit: `feat(cli): expose evidence-based model arena`
6. **Explicit winner application**
   Show diff and require confirmation before cherry-pick/apply. No automatic main-branch write.
   Commit: `feat(arena): apply a verified winner explicitly`
7. **Docs/demo fixture**
   Commit: `docs(arena): add reproducible two-model tournament`

### Phase 3 exit gate

The same baseline is proven for both contestants, failures remain inspectable, cancellation cleans only owned worktrees, and winner selection can be reproduced from journal evidence.

---

# 10. Phase 4 — Causal Time Machine

## Semantic prerequisite

Replay must distinguish:

- **Observation replay:** reuse recorded deterministic tool observations without re-running side effects.
- **Live replay:** explicitly re-execute allowed tools under current policy.
- **Model continuation:** invoke a model from the fork point and expect possible divergence.

Imported capsules default to inspection-only. No live replay without explicit confirmation.

## Task sequence

1. Define `ForkSpec { parent_run_id, parent_sequence, intervention }`.
   Commit: `feat(replay): define explicit run interventions`
2. Build checkpoint materialization from journal + Git reference.
   Commit: `feat(replay): materialize verified fork checkpoints`
3. Support one intervention first: **model substitution**.
   Commit: `feat(replay): fork a run with another model`
4. Add skill/memory/context inclusion masks by stable digest.
   Commit: `feat(replay): ablate recorded context inputs`
5. Compare outcomes using Phase 2 validators.
   Commit: `feat(replay): compare counterfactual outcomes`
6. Implement bounded binary/group bisection over context inputs.
   Stop on budget, nondeterminism threshold, or inconclusive result.
   Commit: `feat(causal): bisect context behind reproducible failures`
7. Generate a causal report with repetitions, confidence wording, cost, and limitations.
   Commit: `feat(causal): report experimentally reproduced causes`
8. Add CLI: `kerux runs fork`, `kerux runs compare`, `kerux runs explain`.
   Commit: `feat(cli): expose causal run debugging`

### Causal-claim acceptance rule

Kerux may say “candidate cause” after one contrasting fork. It may say “reproduced causal factor” only when the configured repeated trials consistently change a deterministic validator outcome. It must say “inconclusive” when model nondeterminism overwhelms the intervention.

---

# 11. Phase 5 — Transactional Swarm

Do not reuse `delegate_to_sub_agent` unchanged. First promote child agents into scoped workers with explicit model, tool registry, worktree, permissions, budget, and journal lineage.

## Task sequence

1. Add scoped `WorkerSpec` and parent/child journal lineage.
2. Add task DAG states: pending, ready, running, prepared, verified, failed, cancelled.
3. Prepare changes in owned worktrees only.
4. Verify every node against local and integration validators.
5. Detect overlapping diffs and semantic conflicts before integration.
6. Introduce a reconciler that produces a candidate in another worktree; never edits main directly.
7. Build an integration commit in a temporary branch.
8. Fast-forward/cherry-pick only after final global gate and user approval.
9. Roll back/retain evidence on failure; clean only owned resources.

Suggested commits:

```text
feat(workers): define scoped coding workers
feat(swarm): persist task dependency states
feat(swarm): prepare verified worktree changes
feat(swarm): detect integration conflicts
feat(swarm): reconcile conflicts in isolation
feat(swarm): integrate verified changes atomically
docs(swarm): document transaction and rollback semantics
```

---

# 12. Phase 6 — Skill Darwinism and Budget Autopilot

## Skill Darwinism

1. Build an explicit regression set from user-approved historical runs only.
2. Run baseline skill and candidate skill against identical forks.
3. Compare required validator outcomes first, then cost/runtime.
4. Hold any candidate with a regression unless the user explicitly accepts it.
5. Promote by atomic skill-file replacement and record promotion evidence.

Never use private historical content in an exported benchmark without explicit inclusion.

Suggested commits:

```text
feat(skills): define approved regression suites
feat(skills): evaluate candidate skills against baseline
feat(skills): gate promotion on non-regression evidence
```

## Outcome Budget Autopilot

Build only after enough measured runs exist. MVP is a transparent heuristic, not an opaque optimizer:

1. Estimate portfolios from local model/task-class outcomes.
2. Reserve budget for verification.
3. Escalate to stronger models only after classified failure/risk.
4. Stop when deterministic gates pass or marginal expected value falls below configured threshold.
5. Show the planned and actual allocation.

Suggested commits:

```text
feat(budget): aggregate verified model outcome profiles
feat(budget): plan transparent model portfolios
feat(budget): stop retries by outcome and spend limits
```

---

## 13. Testing matrix

### Unit

- Redaction and truncation.
- Stable serialization and hash chaining.
- Journal parser and corruption reasons.
- Config defaults/migrations.
- Validator classification.
- Score ordering.
- Intervention masks and bisection stopping.

### Integration

- Crash after each append boundary.
- Cancellation during model stream, approval wait, tool call, and validator.
- Dirty/clean/unborn Git repositories.
- Worktree create/cancel/retain/cleanup.
- Gateway run with existing event streaming plus recorder.
- TUI/non-TUI/autonomous surfaces produce equivalent core journal events.

### Security regression fixtures

Seed fake secrets in:

- Prompt
- Tool JSON
- Shell command
- stdout/stderr
- HTTP headers
- Environment
- Absolute path
- Approval reason

Then scan journal, manifest, capsule JSON, and capsule HTML for every exact secret. Any match fails CI.

### Compatibility

- Existing `Trajectory` JSON remains unchanged during Phase 1.
- Recorder-disabled run behaves like current v0.2.0.
- Existing config without `[recorder]` loads.
- Windows path handling is tested even if first Arena worktree execution is developed on Linux.

---

## 14. Documentation deliverables

Public docs should grow only when the corresponding behavior ships:

```text
docs/src/features/flight-recorder.md
docs/src/features/proof-capsules.md
docs/src/features/evidence-engine.md
docs/src/features/multiverse-arena.md
docs/src/features/causal-debugger.md
docs/src/architecture/run-journal.md
docs/src/security/execution-boundaries.md
docs/src/security/redaction-threat-model.md
```

For each page include:

- What is guaranteed.
- What is not guaranteed.
- Local data paths.
- Config example.
- Exact CLI example.
- Failure/recovery behavior.
- Privacy and export implications.
- Reproducible demo command.

`README.md` remains concise and links to these pages; do not turn it back into a full manual.

---

## 15. Release and rollout gates

### Gate A — internal fixture only

Recorder off by default during the first internal commits if schema may still change.

### Gate B — opt-in public recorder

Enable through config after:

- Secret leakage fixture is green.
- Corruption verifier is stable.
- Storage growth is bounded.
- Gateway/TUI behavior is unchanged.

### Gate C — default local recorder

Consider default-on only after a soak period and explicit retention/storage policy. This is a future decision, not assumed by this plan.

### Gate D — capsule sharing

Do not market “verifiable” until independent import/verify tests pass and docs explain hash-chain limitations.

### Gate E — causal claims

Do not market causal debugging until repeated intervention tests distinguish `candidate`, `reproduced`, and `inconclusive` outcomes.

---

## 16. Open decisions requiring Nix before their phase

1. Real sandbox approach and any new dependency.
2. Recorder default: off, opt-in, or local-redacted-on after soak.
3. Local retention duration/size; intentionally omitted from v1.
4. Whether local prompts are stored redacted or digest-only by default.
5. Failed Arena worktree retention policy.
6. Allowed validators and whether network is disabled during validation.
7. External agent adapter order after native Arena.
8. Whether Proof Capsule uses a directory, single JSON, or approved archive dependency.
9. Optional signing identity after hash-chain MVP.

None of these decisions blocks Tasks 0.1–1.3.

---

## 17. First implementation session boundary

When Nix says to begin, implement **only this slice**:

```text
Task 0.1  truthful execution wording
Task 0.2  pure redactor
Task 0.3  bounded payload
Task 1.1  in-memory journal schema/hash chain
```

Stop after four reviewed commits and the full workspace gate. Do not wire runtime persistence in the same session unless Nix explicitly asks to continue.

Expected commits:

```text
docs(security): clarify host code execution boundaries
feat(security): add deterministic journal redaction
feat(security): bound recorded payloads
feat(recorder): define versioned hash-chained run events
```

This boundary establishes the security and data-contract foundation without creating live user data or changing agent behavior.

---

## 18. Plan completion checklist

- [x] Current source integration points audited.
- [x] Journal separated from legacy trajectory format.
- [x] Privacy/redaction happens before persistence.
- [x] Hash-chain claims distinguished from signatures.
- [x] Replay distinguished from deterministic LLM reproduction.
- [x] Worktree isolation distinguished from OS sandboxing.
- [x] No new dependency required for the first implementation slice.
- [x] Every phase has exit criteria.
- [x] Small staged-commit protocol documented.
- [x] First implementation session is tightly bounded.
- [ ] Nix approves the plan or requests changes.
- [ ] Implementation begins only after explicit approval.

---

## Research references

- Kerux source and roadmap: https://github.com/eikarna/kerux and https://kerux.eikarna.dev/development/roadmap.html
- Hermes Agent features/delegation: https://hermes-agent.nousresearch.com/docs/user-guide/features/overview and https://hermes-agent.nousresearch.com/docs/user-guide/features/delegation
- Aider repo map, lint/test, Git: https://aider.chat/docs/repomap.html, https://aider.chat/docs/usage/lint-test.html, https://aider.chat/docs/git.html
- OpenCode permissions/plugins/agents/server: https://opencode.ai/docs/permissions, https://opencode.ai/docs/plugins, https://opencode.ai/docs/agents, https://opencode.ai/docs/server
- Claude Code checkpointing/teams/sandbox/hooks/remote: https://code.claude.com/docs/en/checkpointing, https://code.claude.com/docs/en/agent-teams, https://code.claude.com/docs/en/sandboxing, https://code.claude.com/docs/en/hooks, https://code.claude.com/docs/en/remote-control
- Orca ADE: https://github.com/stablyai/orca
- LangGraph time travel: https://docs.langchain.com/oss/python/langgraph/use-time-travel
- OpenAI trace grading: https://developers.openai.com/api/docs/guides/trace-grading
- Causal Agent Replay: https://arxiv.org/abs/2606.08275
- LangSmith trajectory evaluation: https://docs.langchain.com/langsmith/trajectory-evals
- Pydantic AI durable execution: https://ai.pydantic.dev/durable_execution/overview

Plan authored from Kerux `main` at `913fb40`; no implementation or commit is part of this plan-only change.
