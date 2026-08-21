//! `kerux runs` — strictly non-executing readers for recorded run journals.

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Subcommand;
use kerux_core::platform::kerux_home;
use kerux_core::run_journal::{JournalError, RunReader, TailState};
use serde_json::json;

/// Stable machine-readable reason codes for failed `runs` commands.
pub mod reason {
    pub const RUN_NOT_FOUND: &str = "run_not_found";
    pub const INVALID_RUN_ID: &str = "invalid_run_id";
    pub const INVALID_MANIFEST: &str = "invalid_manifest";
    pub const CORRUPT_EVENT_LINE: &str = "corrupt_event_line";
    pub const CHAIN_VERIFICATION_FAILED: &str = "chain_verification_failed";
    pub const INCOMPLETE_TAIL: &str = "incomplete_tail";
    pub const IO_ERROR: &str = "io_error";
}

#[derive(Debug, Subcommand)]
pub enum RunsCommands {
    /// List recorded runs under $KERUX_HOME/runs.
    List {
        /// Emit machine-readable JSON (never contains ANSI).
        #[arg(long)]
        json: bool,
    },
    /// Show the manifest and event summary of one run.
    Inspect {
        run_id: String,
        /// Emit machine-readable JSON (never contains ANSI).
        #[arg(long)]
        json: bool,
    },
    /// Re-verify the hash chain of one run without modifying it.
    Verify {
        run_id: String,
        /// Emit machine-readable JSON (never contains ANSI).
        #[arg(long)]
        json: bool,
    },
    /// Export one run as a portable, offline-verifiable proof capsule.
    ///
    /// The capsule is a scrubbed re-chain of the journal: home-directory
    /// paths are replaced with `~`, payloads are re-redacted, and the
    /// capsule carries its own self-consistent hash chain plus per-event
    /// anchors back to the original journal hashes.
    Export {
        run_id: String,
        /// Output file (default: `<run_id>.capsule.html` in the current dir).
        #[arg(long, short)]
        out: Option<PathBuf>,
        /// Emit machine-readable JSON (never contains ANSI).
        #[arg(long)]
        json: bool,
    },
}

pub fn handle(command: &RunsCommands) -> Result<()> {
    match command {
        RunsCommands::List { json } => list_runs(*json),
        RunsCommands::Inspect { run_id, json } => inspect_run(run_id, *json),
        RunsCommands::Verify { run_id, json } => verify_run(run_id, *json),
        RunsCommands::Export { run_id, out, json } => export_run(run_id, out.as_deref(), *json),
    }
}

fn runs_root() -> PathBuf {
    kerux_home().join("runs")
}

fn fail(json: bool, code: &str, message: &str) -> anyhow::Error {
    if json {
        println!(
            "{}",
            json!({ "ok": false, "reason": code, "error": message })
        );
    } else {
        eprintln!("error[{code}]: {message}");
    }
    anyhow::anyhow!("{code}: {message}")
}

fn journal_reason(error: &JournalError) -> &'static str {
    match error {
        JournalError::InvalidRunId(_) => reason::INVALID_RUN_ID,
        JournalError::InvalidManifest(_) => reason::INVALID_MANIFEST,
        JournalError::IncompleteTail => reason::INCOMPLETE_TAIL,
        JournalError::CorruptEventLine { .. } => reason::CORRUPT_EVENT_LINE,
        JournalError::Verification(_) => reason::CHAIN_VERIFICATION_FAILED,
        JournalError::WriterLocked => reason::IO_ERROR,
        JournalError::Io(_) => reason::IO_ERROR,
        JournalError::Json(_) => reason::INVALID_MANIFEST,
    }
}

fn journal_not_found(runs_root: &Path, run_id: &str) -> bool {
    matches!(
        std::fs::symlink_metadata(runs_root.join(run_id)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
}

fn list_runs(json: bool) -> Result<()> {
    let root = runs_root();
    let mut rows = Vec::new();
    match std::fs::read_dir(&root) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if !entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
                    continue;
                }
                let Some(run_id) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                match RunReader::open_in(&root, &run_id) {
                    Ok(reader) => {
                        let manifest = reader.manifest();
                        rows.push(json!({
                            "run_id": manifest.run_id,
                            "status": manifest.status,
                            "created_at_ms": manifest.created_at_ms,
                            "completed_at_ms": manifest.completed_at_ms,
                            "model": manifest.model,
                            "provider_kind": manifest.provider_kind,
                            "surface": manifest.surface,
                            "events": reader.events().len(),
                            "replayability": manifest.replayability,
                            "tail": tail_label(reader.tail_state()),
                        }));
                    }
                    Err(error) => {
                        rows.push(json!({
                            "run_id": run_id,
                            "status": "unreadable",
                            "reason": journal_reason(&error),
                            "error": error.to_string(),
                        }));
                    }
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(fail(json, reason::IO_ERROR, &error.to_string())),
    }
    rows.sort_by(|a, b| {
        let created =
            |value: &serde_json::Value| value.get("created_at_ms").and_then(|v| v.as_u64());
        created(b).cmp(&created(a))
    });

    if json {
        println!("{}", json!({ "ok": true, "runs_root": root, "runs": rows }));
    } else {
        println!("runs root: {}", root.display());
        if rows.is_empty() {
            println!("(no recorded runs)");
        }
        for row in &rows {
            let run_id = row["run_id"].as_str().unwrap_or("?");
            if row["status"] == "unreadable" {
                println!(
                    "{run_id}  UNREADABLE ({})",
                    row["reason"].as_str().unwrap_or("?")
                );
                continue;
            }
            println!(
                "{run_id}  {}  events={}  model={}  created={}",
                row["status"].as_str().unwrap_or("?"),
                row["events"].as_u64().unwrap_or(0),
                row["model"].as_str().unwrap_or("?"),
                row["created_at_ms"].as_u64().unwrap_or(0),
            );
        }
    }
    Ok(())
}

fn inspect_run(run_id: &str, json: bool) -> Result<()> {
    let root = runs_root();
    let reader = match RunReader::open_in(&root, run_id) {
        Ok(reader) => reader,
        Err(error) => {
            let code = if journal_not_found(&root, run_id) {
                reason::RUN_NOT_FOUND
            } else {
                journal_reason(&error)
            };
            return Err(fail(json, code, &error.to_string()));
        }
    };
    let manifest = reader.manifest();
    let mut kinds: std::collections::BTreeMap<&str, u64> = std::collections::BTreeMap::new();
    for event in reader.events() {
        *kinds.entry(event.kind.as_str()).or_insert(0) += 1;
    }

    if json {
        println!(
            "{}",
            json!({
                "ok": true,
                "run_id": manifest.run_id,
                "run_dir": reader.run_dir(),
                "manifest": manifest,
                "tail": tail_label(reader.tail_state()),
                "event_count": reader.events().len(),
                "event_kinds": kinds,
                "events": reader.events(),
            })
        );
    } else {
        println!("run:        {}", manifest.run_id);
        println!("dir:        {}", reader.run_dir().display());
        println!("status:     {:?}", manifest.status);
        println!("replay:     {:?}", manifest.replayability);
        println!("model:      {}", manifest.model);
        println!("provider:   {}", manifest.provider_kind);
        println!("surface:    {}", manifest.surface);
        println!("created:    {}", manifest.created_at_ms);
        if let Some(completed) = manifest.completed_at_ms {
            println!("completed:  {completed}");
        }
        if let Some(head) = manifest.repository_head.as_deref() {
            println!("git head:   {head}");
        }
        println!("tail:       {}", tail_label(reader.tail_state()));
        println!("events:     {}", reader.events().len());
        for (kind, count) in &kinds {
            println!("  {kind}: {count}");
        }
    }
    Ok(())
}

fn verify_run(run_id: &str, json: bool) -> Result<()> {
    let root = runs_root();
    let reader = match RunReader::open_in(&root, run_id) {
        Ok(reader) => reader,
        Err(error) => {
            let code = if journal_not_found(&root, run_id) {
                reason::RUN_NOT_FOUND
            } else {
                journal_reason(&error)
            };
            return Err(fail(json, code, &error.to_string()));
        }
    };
    // RunReader::open_in already chain-verifies; reaching here means the
    // chain, manifest lineage, and event lines all passed.
    let incomplete_tail = reader.tail_state() == TailState::IncompleteTail;
    if json {
        println!(
            "{}",
            json!({
                "ok": true,
                "run_id": run_id,
                "verified": true,
                "events": reader.events().len(),
                "last_hash": reader.events().last().map(|event| event.hash.clone()),
                "incomplete_tail": incomplete_tail,
            })
        );
    } else {
        println!(
            "verified {} events for {run_id} (last hash {})",
            reader.events().len(),
            reader
                .events()
                .last()
                .map(|event| event.hash.as_str())
                .unwrap_or("none")
        );
        if incomplete_tail {
            println!("warning: journal ends with an incomplete final line");
        }
    }
    Ok(())
}

fn export_run(run_id: &str, out: Option<&Path>, json: bool) -> Result<()> {
    use kerux_core::capsule;

    let root = runs_root();
    if journal_not_found(&root, run_id) {
        return Err(fail(
            json,
            reason::RUN_NOT_FOUND,
            &format!("no run '{run_id}' under {}", root.display()),
        ));
    }
    let reader = match RunReader::open_in(&root, run_id) {
        Ok(reader) => reader,
        Err(error) => {
            return Err(fail(json, journal_reason(&error), &error.to_string()));
        }
    };
    let capsule = match capsule::build_capsule(&reader) {
        Ok(capsule) => capsule,
        Err(error) => {
            return Err(fail(json, reason::IO_ERROR, &error.to_string()));
        }
    };
    // Defense in depth: the capsule must verify before we write it out.
    if let Err(error) = capsule::verify_capsule(&capsule) {
        return Err(fail(
            json,
            reason::CHAIN_VERIFICATION_FAILED,
            &error.to_string(),
        ));
    }
    let html = match capsule::render_html(&capsule) {
        Ok(html) => html,
        Err(error) => {
            return Err(fail(json, reason::IO_ERROR, &error.to_string()));
        }
    };
    let out_path = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(format!("{run_id}.capsule.html")));
    if let Err(error) = std::fs::write(&out_path, html) {
        return Err(fail(json, reason::IO_ERROR, &error.to_string()));
    }
    if json {
        println!(
            "{}",
            json!({
                "ok": true,
                "run_id": run_id,
                "capsule_last_hash": capsule.last_hash,
                "events": capsule.events.len(),
                "redacted_events": capsule.redacted_events,
                "path": out_path.display().to_string(),
            })
        );
    } else {
        println!(
            "exported {} events for run '{run_id}' -> {}",
            capsule.events.len(),
            out_path.display()
        );
    }
    Ok(())
}

fn tail_label(state: TailState) -> &'static str {
    match state {
        TailState::Complete => "complete",
        TailState::IncompleteTail => "incomplete_tail",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerux_core::run_journal::{RunJournal, RunManifestV1, RunStatus};
    use serde_json::json;

    fn fixture_manifest(run_id: &str) -> RunManifestV1 {
        RunManifestV1 {
            schema_version: kerux_core::run_journal::SCHEMA_VERSION,
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
            recorder_policy: json!({"max_payload_bytes": 1024}),
            last_sequence: None,
            last_hash: None,
            replayability: kerux_core::run_journal::Replayability::Full,
            warnings: Vec::new(),
        }
    }

    fn temp_root(tag: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let _ = tag;
        dir
    }

    fn seed_run(root: &Path, run_id: &str) {
        let mut journal = RunJournal::create_in(root, fixture_manifest(run_id)).unwrap();
        journal
            .append(1_725_000_000_100, "run_started", json!({"surface": "cli"}))
            .unwrap();
        journal
            .append(
                1_725_000_000_200,
                "tool_start",
                json!({"tool": "read_file"}),
            )
            .unwrap();
        journal
            .finalize(RunStatus::Succeeded, 1_725_000_000_300)
            .unwrap();
    }

    #[test]
    fn reader_lists_seeded_run() {
        let root = temp_root("list");
        seed_run(root.path(), "run-list-1");
        let reader = RunReader::open_in(root.path(), "run-list-1").unwrap();
        assert_eq!(reader.events().len(), 2);
        assert_eq!(reader.manifest().status, RunStatus::Succeeded);
        assert_eq!(reader.tail_state(), TailState::Complete);
    }

    #[test]
    fn corrupt_event_line_is_rejected_with_stable_reason() {
        let root = temp_root("corrupt");
        seed_run(root.path(), "run-corrupt-1");
        let events_path = root.path().join("run-corrupt-1").join("events.ndjson");
        let mut content = std::fs::read_to_string(&events_path).unwrap();
        content.push_str("{not-json}\n");
        std::fs::write(&events_path, content).unwrap();

        let error = RunReader::open_in(root.path(), "run-corrupt-1").unwrap_err();
        assert!(matches!(error, JournalError::CorruptEventLine { .. }));
        assert_eq!(journal_reason(&error), reason::CORRUPT_EVENT_LINE);
    }

    #[test]
    fn broken_hash_chain_is_rejected_with_stable_reason() {
        let root = temp_root("chain");
        seed_run(root.path(), "run-chain-1");
        let events_path = root.path().join("run-chain-1").join("events.ndjson");
        let lines: Vec<serde_json::Value> = std::fs::read_to_string(&events_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let mut tampered = lines[1].clone();
        tampered["payload"]["tool"] = json!("tampered");
        let rebuilt = format!(
            "{}\n{}\n",
            serde_json::to_string(&lines[0]).unwrap(),
            serde_json::to_string(&tampered).unwrap()
        );
        std::fs::write(&events_path, rebuilt).unwrap();

        let error = RunReader::open_in(root.path(), "run-chain-1").unwrap_err();
        assert!(matches!(error, JournalError::Verification(_)));
        assert_eq!(journal_reason(&error), reason::CHAIN_VERIFICATION_FAILED);
    }

    #[test]
    fn missing_run_reports_not_found() {
        let root = temp_root("missing");
        std::fs::create_dir_all(root.path()).unwrap();
        assert!(journal_not_found(root.path(), "run-nope"));
        let error = RunReader::open_in(root.path(), "run-nope").unwrap_err();
        assert!(matches!(error, JournalError::Io(_)));
    }

    #[test]
    fn invalid_run_id_is_rejected() {
        let root = temp_root("invalid");
        let error = RunReader::open_in(root.path(), "../escape").unwrap_err();
        assert!(matches!(error, JournalError::InvalidRunId(_)));
        assert_eq!(journal_reason(&error), reason::INVALID_RUN_ID);
    }
}
