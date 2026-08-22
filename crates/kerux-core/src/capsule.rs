//! Portable, offline-verifiable proof capsules for recorded runs.
//!
//! A capsule is a deterministic, self-contained representation of one run
//! journal that can be shared and verified without access to the original
//! `$KERUX_HOME` storage:
//!
//! - Event payloads and kinds are scrubbed of local filesystem prefixes
//!   (home directories, `KERUX_HOME`) before export.
//! - Scrubbing changes bytes, so the capsule carries its **own** hash chain
//!   recomputed over the scrubbed material using the exact same canonical
//!   serialization as [`crate::run_journal`].
//! - Each event also keeps its `journal_hash` (the hash recorded on disk),
//!   so a holder of the original journal can prove event-by-event
//!   correspondence. Events that needed no scrubbing have
//!   `hash == journal_hash`.
//!
//! The capsule format is deliberately a single JSON document (or a static
//! HTML rendering of it). No archive/ZIP dependency is introduced.

use std::env;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::run_journal::{
    hash_material, RunEventEnvelope, RunManifestV1, RunReader, TailState, SCHEMA_VERSION,
};

/// Discriminator stored in every capsule document.
pub const CAPSULE_FORMAT: &str = "kerux.capsule";

/// Current capsule format version.
pub const CAPSULE_VERSION: u32 = 1;

/// Replacement marker for scrubbed local paths.
pub const PATH_REDACTED: &str = "~";

/// Errors produced while building, rendering, or verifying a capsule.
#[derive(Debug, Error)]
pub enum CapsuleError {
    #[error("capsule verification failed: {0}")]
    Verification(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// One exported event: scrubbed content plus both hash chains.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapsuleEvent {
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub kind: String,
    /// Scrubbed payload content.
    pub payload: Value,
    /// Canonical JSON bytes (UTF-8) digested into `hash`.
    pub hash_material: String,
    /// Capsule-chain link (`None` for the first event).
    pub previous_hash: Option<String>,
    /// Capsule-chain hash over `hash_material`.
    pub hash: String,
    /// Hash recorded in the original run journal for this event.
    pub journal_hash: String,
}

/// A portable, self-verifying evidence bundle for one recorded run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapsuleV1 {
    pub format: String,
    pub capsule_version: u32,
    pub schema_version: u32,
    pub run_id: String,
    pub manifest: RunManifestV1,
    pub events: Vec<CapsuleEvent>,
    pub event_count: usize,
    pub first_hash: Option<String>,
    pub last_hash: Option<String>,
    /// `last_hash` of the original journal chain, for cross-reference.
    pub journal_last_hash: Option<String>,
    /// Number of events whose capsule hash differs from their journal hash
    /// because export scrubbing changed their bytes.
    pub redacted_events: u64,
    pub warnings: Vec<String>,
}

/// Build a deterministic capsule from a chain-verified run reader.
pub fn build_capsule(reader: &RunReader) -> Result<CapsuleV1, CapsuleError> {
    let prefixes = scrub_prefixes();

    let manifest_value = scrub_value(serde_json::to_value(reader.manifest())?, &prefixes);
    let manifest: RunManifestV1 = serde_json::from_value(manifest_value)?;

    let mut events = Vec::with_capacity(reader.events().len());
    let mut chain_previous: Option<String> = None;
    let mut redacted_events = 0u64;

    for event in reader.events() {
        let kind = scrub_text(&event.kind, &prefixes);
        let payload = scrub_value(event.payload.clone(), &prefixes);

        let scrubbed = RunEventEnvelope {
            schema_version: event.schema_version,
            run_id: event.run_id.clone(),
            sequence: event.sequence,
            timestamp_ms: event.timestamp_ms,
            kind: kind.clone(),
            payload: payload.clone(),
            previous_hash: chain_previous.clone(),
            hash: String::new(),
        };
        let material = hash_material(&scrubbed)?;
        let hash = sha256_hex(material.as_bytes());
        if hash != event.hash {
            redacted_events += 1;
        }

        events.push(CapsuleEvent {
            sequence: event.sequence,
            timestamp_ms: event.timestamp_ms,
            kind,
            payload,
            hash_material: material,
            previous_hash: chain_previous.clone(),
            hash: hash.clone(),
            journal_hash: event.hash.clone(),
        });
        chain_previous = Some(hash);
    }

    let mut warnings = manifest.warnings.clone();
    if reader.tail_state() == TailState::IncompleteTail {
        warnings.push("journal ended with an incomplete final line at export time".to_string());
    }

    Ok(CapsuleV1 {
        format: CAPSULE_FORMAT.to_string(),
        capsule_version: CAPSULE_VERSION,
        schema_version: SCHEMA_VERSION,
        run_id: reader.manifest().run_id.clone(),
        first_hash: events.first().map(|event| event.hash.clone()),
        last_hash: chain_previous.clone(),
        journal_last_hash: reader.manifest().last_hash.clone(),
        event_count: events.len(),
        manifest,
        events,
        redacted_events,
        warnings,
    })
}

/// Re-check a capsule's internal consistency and hash chain.
pub fn verify_capsule(capsule: &CapsuleV1) -> Result<(), CapsuleError> {
    let fail = |message: String| Err(CapsuleError::Verification(message));

    if capsule.format != CAPSULE_FORMAT {
        return fail(format!("unknown capsule format {:?}", capsule.format));
    }
    if capsule.capsule_version != CAPSULE_VERSION {
        return fail(format!(
            "unsupported capsule version {}",
            capsule.capsule_version
        ));
    }
    if capsule.events.len() != capsule.event_count {
        return fail(format!(
            "event_count {} does not match {} embedded events",
            capsule.event_count,
            capsule.events.len()
        ));
    }

    let mut previous: Option<String> = None;
    for (index, event) in capsule.events.iter().enumerate() {
        let label = format!("event {index}");
        if event.sequence != index as u64 {
            return fail(format!("{label}: sequence {} out of order", event.sequence));
        }
        if event.previous_hash != previous {
            return fail(format!("{label}: broken chain link"));
        }

        let material: Value = serde_json::from_str(&event.hash_material)
            .map_err(|source| CapsuleError::Verification(format!("{label}: {source}")))?;
        let bound = material["run_id"] == Value::String(capsule.run_id.clone())
            && material["sequence"] == serde_json::json!(event.sequence)
            && material["timestamp_ms"] == serde_json::json!(event.timestamp_ms)
            && material["kind"] == Value::String(event.kind.clone())
            && material["payload"] == event.payload
            && material["previous_hash"]
                == match &event.previous_hash {
                    Some(hash) => Value::String(hash.clone()),
                    None => Value::Null,
                };
        if !bound {
            return fail(format!(
                "{label}: hash material does not match event fields"
            ));
        }

        let digest = sha256_hex(event.hash_material.as_bytes());
        if digest != event.hash {
            return fail(format!("{label}: hash mismatch"));
        }
        previous = Some(event.hash.clone());
    }

    if capsule.first_hash != capsule.events.first().map(|event| event.hash.clone()) {
        return fail("first_hash does not match the first event".to_string());
    }
    if capsule.last_hash != previous {
        return fail("last_hash does not match the final event".to_string());
    }
    Ok(())
}

/// Serialize a capsule as deterministic pretty JSON.
pub fn capsule_json(capsule: &CapsuleV1) -> Result<String, CapsuleError> {
    Ok(serde_json::to_string_pretty(capsule)?)
}

/// Parse a capsule from JSON produced by [`capsule_json`].
pub fn parse_capsule(text: &str) -> Result<CapsuleV1, CapsuleError> {
    Ok(serde_json::from_str(text)?)
}

/// Render a capsule as a fully self-contained static HTML verifier.
///
/// The document embeds the capsule JSON plus a small verifier script. It uses
/// no CDNs, no network fetches, and no external assets; a Content-Security-
/// Policy meta tag forbids every remote source.
pub fn render_html(capsule: &CapsuleV1) -> Result<String, CapsuleError> {
    let json = serde_json::to_string(capsule)?;
    // Keep the JSON blob from ever terminating its <script> element.
    let embedded = json.replace("</", "<\\/");
    let title = format!("kerux proof capsule — {}", capsule.run_id);

    Ok(format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; img-src data:">
<title>{title}</title>
<style>
:root {{ color-scheme: dark; }}
body {{ background:#0d1117; color:#e6edf3; font:14px/1.5 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace; margin:0; padding:24px; }}
h1 {{ font-size:18px; margin:0 0 4px; }}
h2 {{ font-size:14px; margin:24px 0 8px; color:#8b949e; text-transform:uppercase; letter-spacing:.08em; }}
.badge {{ display:inline-block; padding:2px 10px; border-radius:999px; font-weight:700; font-size:12px; }}
.badge.pending {{ background:#1f2937; color:#9ca3af; }}
.badge.ok {{ background:#052e16; color:#4ade80; border:1px solid #14532d; }}
.badge.bad {{ background:#3f0d0d; color:#f87171; border:1px solid #7f1d1d; }}
.badge.info {{ background:#172554; color:#93c5fd; border:1px solid #1e3a8a; }}
table {{ border-collapse:collapse; width:100%; margin:8px 0; }}
th,td {{ border:1px solid #21262d; padding:4px 8px; text-align:left; vertical-align:top; }}
th {{ color:#8b949e; font-weight:600; white-space:nowrap; }}
td.hash {{ font-size:12px; color:#7ee787; word-break:break-all; }}
details {{ margin:2px 0; }}
summary {{ cursor:pointer; color:#8b949e; }}
pre {{ background:#161b22; border:1px solid #21262d; padding:8px; overflow:auto; max-height:320px; white-space:pre-wrap; word-break:break-word; }}
.muted {{ color:#8b949e; }}
#warnings li {{ color:#f0b429; }}
</style>
</head>
<body>
<h1>Kerux Proof Capsule</h1>
<p>
  run <strong id="run-id"></strong>
  &middot; integrity <span id="integrity-badge" class="badge pending">VERIFYING…</span>
  &middot; replayability <span id="replay-badge" class="badge info"></span>
  &middot; status <span id="status-badge" class="badge info"></span>
</p>
<p class="muted" id="detail"></p>
<ul id="warnings"></ul>

<h2>Manifest</h2>
<table id="manifest-table"><tbody></tbody></table>

<h2>Events</h2>
<table>
<thead><tr><th>#</th><th>kind</th><th>timestamp (ms)</th><th>hash</th><th>payload</th></tr></thead>
<tbody id="events-body"></tbody>
</table>

<p class="muted">
This file is fully self-contained: no network access, no external assets.
Integrity is re-computed locally by re-hashing each event&rsquo;s canonical
material (SHA-256) and re-linking the chain. Events scrubbed for export are
re-chained; their original journal hashes are kept for cross-reference.
</p>

<script id="capsule-data" type="application/json">{embedded}</script>
<script>
"use strict";
/*SHA256-START*/
function sha256fallback(bytes) {{
  var K = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2
  ];
  function rotr(x, n) {{ return (x >>> n) | (x << (32 - n)); }}
  var H = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
    0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19
  ];
  var data = [];
  for (var i = 0; i < bytes.length; i++) data.push(bytes[i]);
  var bitLen = bytes.length * 8;
  data.push(0x80);
  while (data.length % 64 !== 56) data.push(0);
  for (var s = 56; s >= 0; s -= 8) data.push(Math.floor(bitLen / Math.pow(2, s)) & 0xff);
  var w = new Array(64);
  for (var off = 0; off < data.length; off += 64) {{
    for (var t = 0; t < 16; t++) {{
      w[t] = (data[off + t * 4] << 24) | (data[off + t * 4 + 1] << 16) |
             (data[off + t * 4 + 2] << 8) | data[off + t * 4 + 3];
    }}
    for (var t = 16; t < 64; t++) {{
      var s0 = rotr(w[t - 15], 7) ^ rotr(w[t - 15], 18) ^ (w[t - 15] >>> 3);
      var s1 = rotr(w[t - 2], 17) ^ rotr(w[t - 2], 19) ^ (w[t - 2] >>> 10);
      w[t] = (w[t - 16] + s0 + w[t - 7] + s1) | 0;
    }}
    var a = H[0], b = H[1], c = H[2], d = H[3], e = H[4], f = H[5], g = H[6], h = H[7];
    for (var t = 0; t < 64; t++) {{
      var S1 = rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25);
      var ch = (e & f) ^ (~e & g);
      var temp1 = (h + S1 + ch + K[t] + w[t]) | 0;
      var S0 = rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22);
      var maj = (a & b) ^ (a & c) ^ (b & c);
      var temp2 = (S0 + maj) | 0;
      h = g; g = f; f = e; e = (d + temp1) | 0;
      d = c; c = b; b = a; a = (temp1 + temp2) | 0;
    }}
    H[0] = (H[0] + a) | 0; H[1] = (H[1] + b) | 0; H[2] = (H[2] + c) | 0; H[3] = (H[3] + d) | 0;
    H[4] = (H[4] + e) | 0; H[5] = (H[5] + f) | 0; H[6] = (H[6] + g) | 0; H[7] = (H[7] + h) | 0;
  }}
  var out = "";
  for (var i = 0; i < 8; i++) {{
    for (var j = 7; j >= 0; j--) out += ((H[i] >>> (j * 4)) & 0xf).toString(16);
  }}
  return out;
}}
/*SHA256-END*/

function sha256hex(text) {{
  var bytes = new TextEncoder().encode(text);
  if (window.crypto && window.crypto.subtle && window.crypto.subtle.digest) {{
    return window.crypto.subtle.digest("SHA-256", bytes).then(function (buf) {{
      var out = "";
      var view = new Uint8Array(buf);
      for (var i = 0; i < view.length; i++) out += view[i].toString(16).padStart(2, "0");
      return out;
    }});
  }}
  return Promise.resolve(sha256fallback(bytes));
}}

function same(a, b) {{ return (a === null ? null : a) === (b === null ? null : b); }}

function setText(id, value) {{
  var el = document.getElementById(id);
  if (el) el.textContent = String(value);
}}

function setBadge(id, cls, text) {{
  var el = document.getElementById(id);
  if (!el) return;
  el.className = "badge " + cls;
  el.textContent = text;
}}

function fail(message) {{
  setBadge("integrity-badge", "bad", "TAMPERED");
  setText("detail", "Verification failed: " + message);
}}

(function () {{
  var capsule;
  try {{
    capsule = JSON.parse(document.getElementById("capsule-data").textContent);
  }} catch (error) {{
    fail("embedded capsule is not valid JSON");
    return;
  }}

  setText("run-id", capsule.run_id);
  setBadge("replay-badge", "info", String(capsule.manifest.replayability));
  setBadge("status-badge", "info", String(capsule.manifest.status));

  var warningsList = document.getElementById("warnings");
  (capsule.warnings || []).forEach(function (warning) {{
    var li = document.createElement("li");
    li.textContent = warning;
    warningsList.appendChild(li);
  }});

  var manifest = capsule.manifest;
  var rows = [
    ["run id", manifest.run_id],
    ["schema version", manifest.schema_version],
    ["status", manifest.status],
    ["replayability", manifest.replayability],
    ["surface", manifest.surface],
    ["model", manifest.model],
    ["provider", manifest.provider_kind],
    ["created (ms)", manifest.created_at_ms],
    ["completed (ms)", manifest.completed_at_ms === null ? "—" : manifest.completed_at_ms],
    ["git head", manifest.repository_head === null ? "—" : manifest.repository_head],
    ["git branch", manifest.repository_branch === null ? "—" : manifest.repository_branch],
    ["workspace fingerprint", manifest.workspace_fingerprint],
    ["journal last hash", capsule.journal_last_hash === null ? "—" : capsule.journal_last_hash],
    ["events scrubbed for export", capsule.redacted_events]
  ];
  var tbody = document.querySelector("#manifest-table tbody");
  rows.forEach(function (row) {{
    var tr = document.createElement("tr");
    var th = document.createElement("th");
    th.textContent = row[0];
    var td = document.createElement("td");
    td.textContent = String(row[1]);
    tr.appendChild(th);
    tr.appendChild(td);
    tbody.appendChild(tr);
  }});

  var eventsBody = document.getElementById("events-body");
  capsule.events.forEach(function (event) {{
    var tr = document.createElement("tr");
    var cells = [event.sequence, event.kind, event.timestamp_ms];
    cells.forEach(function (value) {{
      var td = document.createElement("td");
      td.textContent = String(value);
      tr.appendChild(td);
    }});
    var hashTd = document.createElement("td");
    hashTd.className = "hash";
    hashTd.textContent = event.hash;
    tr.appendChild(hashTd);
    var payloadTd = document.createElement("td");
    var details = document.createElement("details");
    var summary = document.createElement("summary");
    summary.textContent = "payload";
    var pre = document.createElement("pre");
    pre.textContent = JSON.stringify(event.payload, null, 2);
    details.appendChild(summary);
    details.appendChild(pre);
    payloadTd.appendChild(details);
    tr.appendChild(payloadTd);
    eventsBody.appendChild(tr);
  }});

  (async function verify() {{
    var previous = null;
    var redacted = 0;
    for (var i = 0; i < capsule.events.length; i++) {{
      var event = capsule.events[i];
      if (event.sequence !== i) {{ fail("sequence gap at event " + i); return; }}
      if (!same(event.previous_hash, previous)) {{ fail("broken chain link at event " + i); return; }}
      var material;
      try {{ material = JSON.parse(event.hash_material); }}
      catch (error) {{ fail("unparseable hash material at event " + i); return; }}
      var materialPrevious = material.previous_hash === null ? null : material.previous_hash;
      var eventPrevious = event.previous_hash === null ? null : event.previous_hash;
      if (material.run_id !== capsule.run_id ||
          material.sequence !== event.sequence ||
          material.timestamp_ms !== event.timestamp_ms ||
          material.kind !== event.kind ||
          JSON.stringify(material.payload) !== JSON.stringify(event.payload) ||
          materialPrevious !== eventPrevious) {{
        fail("hash material does not match event fields at event " + i);
        return;
      }}
      var digest = await sha256hex(event.hash_material);
      if (digest !== event.hash) {{ fail("hash mismatch at event " + i); return; }}
      if (event.hash !== event.journal_hash) redacted++;
      previous = event.hash;
    }}
    if (capsule.last_hash !== previous) {{ fail("last_hash does not match final event"); return; }}
    if (redacted !== capsule.redacted_events) {{ fail("redacted event count mismatch"); return; }}
    setBadge("integrity-badge", "ok", "VERIFIED");
    setText("detail", capsule.event_count + " events verified locally (SHA-256 chain intact" +
      (redacted > 0 ? "; " + redacted + " event(s) scrubbed for export and re-chained" : "") + ").");
  }})();
}})();
</script>
</body>
</html>
"##,
        title = title,
        embedded = embedded,
    ))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Local filesystem prefixes that must never leave the machine.
fn scrub_prefixes() -> Vec<String> {
    let mut prefixes: Vec<String> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        prefixes.push(home.to_string_lossy().into_owned());
    }
    for var in ["KERUX_HOME", "HOME", "USERPROFILE"] {
        if let Ok(value) = env::var(var) {
            if !value.is_empty() {
                prefixes.push(value);
            }
        }
    }
    // Longest first so nested prefixes collapse in one pass.
    prefixes.sort_by_key(|prefix| std::cmp::Reverse(prefix.len()));
    prefixes.dedup();
    prefixes.retain(|prefix| prefix.len() > 1);
    prefixes
}

fn scrub_text(text: &str, prefixes: &[String]) -> String {
    let mut out = text.to_string();
    for prefix in prefixes {
        if out.contains(prefix) {
            out = out.replace(prefix.as_str(), PATH_REDACTED);
        }
    }
    out
}

fn scrub_value(value: Value, prefixes: &[String]) -> Value {
    match value {
        Value::String(text) => Value::String(scrub_text(&text, prefixes)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| scrub_value(item, prefixes))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, item)| (key, scrub_value(item, prefixes)))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_journal::{RunJournal, RunStatus};
    use serde_json::json;

    fn fixture_manifest(run_id: &str) -> RunManifestV1 {
        RunManifestV1 {
            schema_version: SCHEMA_VERSION,
            run_id: run_id.to_string(),
            parent_run_id: None,
            parent_sequence: None,
            created_at_ms: 1_725_000_000_000,
            completed_at_ms: None,
            status: RunStatus::Running,
            surface: "cli".to_string(),
            model: "test-model".to_string(),
            provider_kind: "test-provider".to_string(),
            workspace_fingerprint: "workspace-sha256".to_string(),
            repository_head: None,
            repository_dirty_hash: None,
            repository_branch: None,
            repository_clean: None,
            repository_changed_files: Vec::new(),
            recorder_policy: json!({"max_payload_bytes": 4096}),
            last_sequence: None,
            last_hash: None,
            replayability: crate::run_journal::Replayability::Full,
            warnings: Vec::new(),
        }
    }

    fn seed_run(root: &std::path::Path, run_id: &str, payload: Value) {
        let mut journal = RunJournal::create_in(root, fixture_manifest(run_id)).unwrap();
        journal
            .append(1_725_000_000_100, "run_started", json!({"surface": "cli"}))
            .unwrap();
        journal
            .append(1_725_000_000_200, "tool_start", payload)
            .unwrap();
        journal
            .finalize(RunStatus::Succeeded, 1_725_000_000_300)
            .unwrap();
    }

    #[test]
    fn capsule_builds_verifies_and_is_deterministic() {
        let root = tempfile::tempdir().unwrap();
        seed_run(root.path(), "run-capsule-ok", json!({"tool": "read_file"}));
        let reader = RunReader::open_in(root.path(), "run-capsule-ok").unwrap();

        let capsule = build_capsule(&reader).unwrap();
        verify_capsule(&capsule).unwrap();
        assert_eq!(capsule.event_count, 2);
        assert_eq!(capsule.redacted_events, 0);
        assert_eq!(capsule.last_hash, reader.manifest().last_hash);

        let again = build_capsule(&reader).unwrap();
        assert_eq!(
            capsule_json(&capsule).unwrap(),
            capsule_json(&again).unwrap()
        );

        let parsed = parse_capsule(&capsule_json(&capsule).unwrap()).unwrap();
        verify_capsule(&parsed).unwrap();
        assert_eq!(parsed, capsule);
    }

    #[test]
    #[serial_test::serial]
    fn home_paths_are_scrubbed_and_chain_still_verifies() {
        let home = dirs::home_dir().unwrap();
        let leaky = home.join("secret-project/main.rs");
        let root = tempfile::tempdir().unwrap();
        seed_run(
            root.path(),
            "run-capsule-scrub",
            json!({"path": leaky.to_string_lossy(), "note": "plain"}),
        );
        let reader = RunReader::open_in(root.path(), "run-capsule-scrub").unwrap();

        let capsule = build_capsule(&reader).unwrap();
        verify_capsule(&capsule).unwrap();

        let exported = capsule_json(&capsule).unwrap();
        assert!(
            !exported.contains(&home.to_string_lossy().into_owned()),
            "exported capsule leaks the home directory"
        );
        assert!(exported.contains(PATH_REDACTED));
        assert!(capsule.redacted_events >= 1);
        // The scrubbed event must carry its original journal hash anchor.
        let scrubbed = capsule
            .events
            .iter()
            .find(|event| event.hash != event.journal_hash)
            .expect("at least one re-chained event");
        assert!(scrubbed.journal_hash.len() == 64);
    }

    #[test]
    fn tampered_capsule_fails_verification() {
        let root = tempfile::tempdir().unwrap();
        seed_run(root.path(), "run-capsule-tamper", json!({"tool": "patch"}));
        let reader = RunReader::open_in(root.path(), "run-capsule-tamper").unwrap();

        let mut capsule = build_capsule(&reader).unwrap();
        capsule.events[1].payload = json!({"tool": "tampered"});
        let error = verify_capsule(&capsule).unwrap_err();
        assert!(matches!(error, CapsuleError::Verification(_)));

        let mut capsule = build_capsule(&reader).unwrap();
        capsule.events[1].hash_material.push(' ');
        assert!(verify_capsule(&capsule).is_err());

        let mut capsule = build_capsule(&reader).unwrap();
        capsule.last_hash = Some("0".repeat(64));
        assert!(verify_capsule(&capsule).is_err());
    }

    #[test]
    #[serial_test::serial]
    fn html_export_is_offline_clean_and_embeds_verifiable_data() {
        let home = dirs::home_dir().unwrap();
        let root = tempfile::tempdir().unwrap();
        seed_run(
            root.path(),
            "run-capsule-html",
            json!({"path": home.join("x.txt").to_string_lossy()}),
        );
        let reader = RunReader::open_in(root.path(), "run-capsule-html").unwrap();
        let capsule = build_capsule(&reader).unwrap();

        let html = render_html(&capsule).unwrap();

        for marker in [
            "http://",
            "https://",
            "fetch(",
            "XMLHttpRequest",
            "<script src",
            "link rel",
            "@import",
            "url(",
        ] {
            assert!(
                !html.contains(marker),
                "html contains network marker {marker}"
            );
        }
        assert!(html.contains("Content-Security-Policy"));
        assert!(html.contains("id=\"integrity-badge\""));
        assert!(html.contains("id=\"capsule-data\""));
        assert!(
            !html.contains(&home.to_string_lossy().into_owned()),
            "html leaks the home directory"
        );

        // The embedded blob must round-trip back into a verifiable capsule.
        let start =
            html.find("type=\"application/json\">").unwrap() + "type=\"application/json\">".len();
        let end = html[start..].find("</script>").unwrap() + start;
        let embedded = html[start..end].replace("<\\/", "</");
        let parsed = parse_capsule(&embedded).unwrap();
        verify_capsule(&parsed).unwrap();
        assert_eq!(parsed, capsule);
    }
}
