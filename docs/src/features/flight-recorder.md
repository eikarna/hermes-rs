# Flight Recorder & Proof Capsules

Kerux can journal every agent run into a **tamper-evident local flight
recorder**: a hash-chained event log you can inspect, verify, and export
as a portable, offline-verifiable **proof capsule**.

The recorder answers one question after any incident: *what exactly did
the agent do, in what order, and can I prove this log was not edited?*

---

## Enabling the recorder

The recorder is **on by default**. Tune it in your config:

```toml
[recorder]
enabled = true                  # default; set false to disable journaling
# max_payload_bytes = 65536     # per-event payload cap (redacted, bounded)
# record_content = true         # default; assistant/tool bodies (still redacted)
# record_reasoning = false      # default; reasoning bodies: metadata only
# failure_mode = "warn"         # "warn" = log & continue, "fail" = abort run
```

With `failure_mode = "warn"` a journal I/O problem never breaks your
session; the run continues and the incident is logged. Use `"fail"` when
an unrecorded run is unacceptable (e.g. compliance contexts).

## What gets recorded

Each run creates a directory under `$KERUX_HOME/runs/<run_id>/`
containing a versioned **manifest** and an append-only NDJSON event
stream. Every event carries a SHA-256 hash chained to its predecessor,
so any edit, deletion, or truncation breaks verification.

Main event vocabulary:

| Event | Meaning |
|---|---|
| `run_started` / `run_completed` / `run_cancelled` / `run_failed` | Run lifecycle, with exactly one terminal status |
| `request_prepared` | Full request provenance: model, provider, context composition |
| `tool_started` / `tool_completed` / `tool_failed` | Tool execution timeline, correlated by call id |
| `approval_decision` | Human/tool-approval decisions with redacted reasons |
| `edit_outcome` | Edit protocol outcome: format, parse/apply status, pass kind (`first_pass` vs `repair_pass`), repair counts, effective routing format |
| `validator_result` | Project-validator evidence: command digest, exit code, bounded/redacted output |

Git checkpoint metadata (repository HEAD, dirty-tree patch hash) is
attached to the manifest at snapshot points.

All payloads are **redacted and size-bounded** before they touch disk,
independently of the `[recorder]` caps. Raw provider reasoning bodies are
only stored if `record_reasoning = true`; otherwise just metadata.

## Inspecting runs — read-only by construction

`kerux runs` commands **never execute anything**. They open journals
through a strictly read-only reader that verifies the hash chain while
parsing and detects a crash-truncated tail instead of failing mysteriously.

```bash
kerux runs list                 # newest first, human-readable
kerux runs list --json          # machine-readable, no ANSI, ever
kerux runs inspect <run_id>     # manifest + full event timeline
kerux runs verify <run_id>      # re-verify the chain, modify nothing
```

Example `--json` shapes (abbreviated):

```json
{ "ok": true, "runs_root": "/home/me/.kerux/runs", "runs": [
  { "run_id": "01J…", "status": "completed", "events": 42,
    "model": "…", "provider_kind": "openai", "surface": "cli",
    "replayability": "…", "tail": "complete" } ] }
```

Failed commands emit stable machine-readable reason codes —
`run_not_found`, `corrupt_event_line`, `chain_verification_failed`,
`incomplete_tail`, … — so scripts can branch on them without parsing
prose.

## Proof capsules — the shareable form

A journal is **local, full-fidelity, and private**. To share evidence,
export a capsule:

```bash
kerux runs export <run_id>                 # writes <run_id>.capsule.html
kerux runs export <run_id> --out proof.html --json
```

Export **verifies the source chain first** and refuses to produce a
capsule from a broken journal. The capsule is a *scrubbed re-chain*:

- home-directory paths replaced with `~`,
- payloads re-redacted and re-bounded,
- its own self-consistent SHA-256 chain (**capsule version 1**),
- per-event anchor hashes back to the original journal,
- packaged as a single self-contained HTML file.

Anyone can re-verify a capsule offline — no Kerux install, no network,
no keys. Open it in any HTML-capable viewer or feed it back to the
verify tooling.

## What this is — and what it is not

The recorder's guarantees are easy to overstate, so here are the four
distinctions that matter.

### Hash chain ≠ signature

The SHA-256 chain gives **tamper *evidence***: any modification after
the fact is detectable. It does **not** give authenticity or
non-repudiation. Anyone with filesystem access can rewrite a journal
*and re-chain it*; nothing in the file proves who wrote it. Cryptographic
signatures (keys, identity) are explicitly out of scope for v1. Treat
chain verification as *integrity checking*, not *proof of origin*.

### Replay ≠ deterministic reproduction

The journal records **observations** for causal debugging. The manifest's
replayability field marks whether inputs were captured well enough to
*attempt* a reproduction — it is not a VM snapshot. Replaying against a
live provider can legitimately diverge (sampling non-determinism, wall
clock, external side effects like git state or network). Evidence first;
deterministic reproduction is a separate, harder problem.

### Git worktree harness ≠ sandbox

The transactional git harness (pre-run snapshots, checkpoints, `/undo`)
operates on your **real working tree** — it protects against *mistakes*,
not against malicious code. Validators run in a lexically confined
working directory with output caps, but confinement is **not a security
sandbox** against hostile programs. Accordingly: capsules never execute
anything, and running validators from an *imported* run requires
explicit user action, every time.

### Local journal ≠ shareable capsule

| | Journal (`$KERUX_HOME/runs`) | Proof capsule (`.capsule.html`) |
|---|---|---|
| Audience | You, on this machine | Other people/machines |
| Fidelity | Full (within redaction/bounds) | Scrubbed re-chain |
| Paths | Absolute home paths | `~`-substituted |
| Chain | Original event chain | Own chain v1 + anchors to original |
| Sharing | Never share raw | Designed for sharing |

Even though capsules are redacted, treat both artifacts as sensitive:
pattern-based redaction is best-effort, not a data-loss guarantee.

## Schema stability

Consumers (scripts, dashboards, future importers) rely on:

- versioned manifest schema and `CAPSULE_VERSION = 1`,
- stable machine-readable reason codes from `kerux runs`,
- read-only readers that never mutate journals,
- additive evolution: new event kinds may appear; existing kinds keep
  their payload shape.

Breaking changes bump versions rather than mutating meanings.

## Reproducible demo

End-to-end, five commands:

```bash
# 1. enable recording, then run anything through Kerux
#    (a scripted chat turn works fine as a fixture)

# 2. find the run
kerux runs list --json | jq -r '.runs[0].run_id'

# 3. walk the timeline
kerux runs inspect "$RUN_ID"

# 4. prove integrity
kerux runs verify "$RUN_ID"

# 5. export and re-verify the capsule offline
kerux runs export "$RUN_ID"
```

Leakage check for the fixture: the exported capsule must contain no
absolute home paths and no seeded secret strings — search the HTML for
both before treating the pipeline as trusted.
