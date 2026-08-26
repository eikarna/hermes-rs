# Taste/Style Profile Learning

Kerux learns portable, confidence-scored coding-style preferences from your
trajectory history and injects them into the system prompt — so the agent
writes code the way you do, across every project.

Inspired by CommandCode's taste registry: preferences like
`export style: named exports (confidence 0.85)` travel with the user, not
the repo. Design reference: `research/product-research-t_2be9e216.md`
(ide #9).

## Data model

Everything lives in `crates/kerux-core/src/taste.rs`.

| Type | Role |
|------|------|
| `TasteProfile` | Portable unit: versioned bag of preferences + metadata. The JSON document *is* the profile — push/pull needs no conversion step. |
| `TastePreference` | One learned rule: stable `key` (`export style`), preferred `value` (`named exports`), evidence counters (`positive`/`negative`), denormalized `confidence` in `0.0..=1.0`. |
| `PreferenceObservation` | One extracted signal from trajectory history: "the user's work exhibited (or contradicted) preference X", with weight and timestamp. |
| `PreferenceExtractor` | Trait: `&[Trajectory] -> Vec<PreferenceObservation>`. The extraction engine implements this. |
| `TasteStore` / `FileTasteStore` | Portable storage contract + default file-backed implementation (one JSON document per profile name). |

Supporting enums: `PreferenceCategory` (naming, formatting, architecture,
tooling, language, documentation, testing, workflow, other) and
`PreferenceSource` (extracted, inferred, manual).

## Confidence scoring

Every preference counts supporting (`positive`) and contradicting
(`negative`) observations. Confidence combines two factors
(`compute_confidence`):

- **Consistency** — `positive / (positive + negative)`. A preference that
  is contradicted half the time can never exceed `0.5`.
- **Saturation** — `n / (n + HALF_SATURATION)` with `HALF_SATURATION = 5`.
  One observation yields `~0.17`, five yield `0.5`, twenty yield `0.8`;
  confidence grows slowly and never quite reaches `1.0`.

```text
confidence = (positive / total) * (total / (total + 5))
```

No evidence scores `0.0` — nothing is injected until something is
actually observed. The score is stored denormalized on the preference and
recomputed whenever evidence changes (`recompute_confidence`); the raw
counters are the source of truth.

## Storage format

The portable JSON document is the profile itself (`version` field guards
forward compatibility, missing fields get defaults so old documents stay
readable):

```json
{
  "version": 1,
  "name": "kerux",
  "created_at": 1787700000,
  "updated_at": 1787766000,
  "preferences": [
    {
      "key": "export style",
      "category": "language",
      "value": "named exports",
      "positive": 20,
      "negative": 1,
      "confidence": 0.8,
      "source": "extracted",
      "first_observed_at": 1787700000,
      "last_observed_at": 1787766000
    }
  ],
  "metadata": { "source_project": "kerux" }
}
```

Two locations, same format:

- **Store (portable registry)**: `FileTasteStore` persists one pretty-JSON
  document per profile name under `<data_root>/taste/<name>.json`
  (`~/.kerux/taste/`, `KERUX_HOME` overrides the root). Names are
  sanitized for the filesystem.
- **Project-local**: `<project_root>/.kerux/taste.json`
  (`project_taste_path`) — commit it to version control to share a house
  style with your team via PR.

Writes are atomic (temp file + rename, via the shared `persist` helpers);
missing or corrupt files read as "start fresh".

## Push/pull semantics

Push = load the project profile, save it into a `TasteStore` under a
name. Pull = load from the store, `TasteProfile::merge` into the project
profile. Merge rules, per matching `key`:

- **Same value**: evidence counters add, the observation window widens
  (min first / max last), confidence recomputed.
- **Conflicting values**: the side with more total evidence wins (ties go
  to the more recently observed one).
- **Manual source propagates**: an explicitly stated preference
  (`source: manual`) wins over learned sources.
- Keys present on only one side are copied over; metadata keys missing
  from the target are filled in without overwriting.

## Prompt injection

`TasteProfile::render_prompt_block(min_confidence, max_items)` renders
the top preferences (highest confidence first, at most one entry per key,
capped) as a markdown block:

```text
## Learned Coding Style Preferences

Learned from past sessions. Follow these unless the user instructs otherwise.

- export style: named exports (confidence 0.80)
- naming: snake_case files (confidence 0.75)
```

Returns `None` when nothing clears the threshold or `max_items == 0` —
callers omit the block entirely rather than injecting an empty section.
`retain_confident(min)` prunes weak preferences from a profile outright.

## Division of labor

- `taste.rs` (this design): schema, scoring math, storage, merge, prompt
  rendering.
- Extraction engine (trajectories → observations): implements
  `PreferenceExtractor`, folds results via
  `TasteProfile::apply_observations`.
- System-prompt wiring and `kerux taste push/pull` CLI: built on
  `TasteStore` + `render_prompt_block` + `project_taste_path`.
