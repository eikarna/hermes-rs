//! Project validator execution with journal evidence capture (Task 2.2).
//!
//! Executes a [`ValidationPolicy`]'s validators in declaration order,
//! captures bounded + redacted output per validator, journals
//! `validator_result` / `validation_pass` evidence events when a run
//! recorder is attached, and aggregates a [`ValidationPassResult`].
//!
//! Safety model (mirrors `validation.rs` config-time checks):
//! - No shell. `argv[0]` is resolved through `PATH` only when it is a bare
//!   program name; anything containing a path separator is rejected.
//! - Working directories are confined under the workspace root.
//! - Captured output is hard-capped and passed through secret redaction
//!   before it ever lands in memory beyond the cap or in the journal.

use crate::redaction::redact_text;
use crate::run_journal::RunRecorder;
use crate::validation::{
    ValidationPassResult, ValidationPolicy, ValidatorOutcome, ValidatorResult, ValidatorSpec,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tracing::{info, warn};

/// Absolute ceiling for captured output, even if a spec asks for more.
const MAX_OUTPUT_CAP_BYTES: usize = 64 * 1024;

/// Read budget used while draining process pipes (slightly above the cap so
/// truncation is detectable rather than silently clipped by the read loop).
const READ_CHUNK_BYTES: usize = MAX_OUTPUT_CAP_BYTES + 4096;

fn unix_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Whether `program` may be spawned directly. Only bare program names are
/// allowed — they resolve through `PATH`. Paths (`./x`, `/bin/sh`, `..\x`)
/// are refused so config cannot smuggle arbitrary executables past the
/// declared workspace boundary.
fn program_allowed(program: &str) -> bool {
    !program.is_empty()
        && !program.contains('/')
        && !program.contains('\\')
        && program != "."
        && program != ".."
}

/// Resolve `spec.workdir` under `workspace`, refusing anything that escapes.
///
/// Defense in depth: `ValidatorSpec::validate()` already rejects absolute
/// paths and traversal components at config-load time; this re-checks the
/// lexical join (normalized first, so `..` segments cannot hide past the
/// `starts_with` containment check) and, when the directory exists, the
/// canonicalized location.
fn lexically_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                // Pop the previous component; if we are already at the root,
                // keep the ParentDir so the containment check below fails
                // loudly instead of silently clamping to the workspace root.
                if !normalized.pop() {
                    normalized.push(component);
                }
            }
            std::path::Component::CurDir => {}
            other => normalized.push(other),
        }
    }
    normalized
}

fn confined_workdir(spec: &ValidatorSpec, workspace: &Path) -> Result<PathBuf, String> {
    let joined = lexically_normalize(&spec.resolved_workdir(workspace));

    // Lexical containment (works even if the directory does not exist yet).
    if !joined.starts_with(workspace) {
        return Err(format!(
            "workdir {:?} escapes the workspace root",
            spec.workdir
        ));
    }

    // Canonical containment when possible (rejects symlink escapes).
    if joined.exists() {
        if let (Ok(canonical_dir), Ok(canonical_ws)) =
            (joined.canonicalize(), workspace.canonicalize())
        {
            if !canonical_dir.starts_with(&canonical_ws) {
                return Err(format!("workdir {:?} escapes the workspace", spec.workdir));
            }
        }
    }

    Ok(joined)
}

fn skipped_result(spec_name: &str, reason: &str) -> ValidatorResult {
    ValidatorResult {
        name: spec_name.to_string(),
        outcome: ValidatorOutcome::Skipped,
        exit_code: None,
        duration_ms: 0,
        output: format!("skipped: {reason}"),
        output_truncated: false,
    }
}

fn spawn_error_result(spec_name: &str, reason: String) -> ValidatorResult {
    ValidatorResult {
        name: spec_name.to_string(),
        outcome: ValidatorOutcome::SpawnError,
        exit_code: None,
        duration_ms: 0,
        output: format!("spawn error: {reason}"),
        output_truncated: false,
    }
}

/// Truncate raw process output to the effective cap, redact secrets, and
/// report whether truncation happened.
fn bound_and_redact(raw: &[u8], cap_bytes: usize) -> (String, bool) {
    let cap = cap_bytes.min(MAX_OUTPUT_CAP_BYTES);
    let truncated = raw.len() > cap;
    let clipped = if truncated { &raw[..cap] } else { raw };
    (redact_text(&String::from_utf8_lossy(clipped)), truncated)
}

/// Execute one validator and capture bounded evidence. Never panics; every
/// failure mode maps onto a [`ValidatorOutcome`].
async fn run_validator(spec: &ValidatorSpec, workspace: &Path) -> ValidatorResult {
    let started = Instant::now();

    let argv = spec.argv();
    let Some(program) = argv.first().copied() else {
        return spawn_error_result(&spec.name, "empty command".to_string());
    };
    if !program_allowed(program) {
        return spawn_error_result(
            &spec.name,
            format!("program {program:?} is not a bare name; path separators are not allowed"),
        );
    }

    let dir = match confined_workdir(spec, workspace) {
        Ok(dir) => dir,
        Err(reason) => return spawn_error_result(&spec.name, reason),
    };

    let mut cmd = Command::new(program);
    cmd.args(&argv[1..])
        .current_dir(&dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            return spawn_error_result(&spec.name, format!("failed to spawn {program:?}: {error}"))
        }
    };

    // Drain pipes concurrently so a chatty validator cannot deadlock on a
    // full pipe buffer, with a combined ceiling well above the per-spec cap.
    let mut stdout_pipe = child.stdout.take().expect("stdout piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr piped");
    let drain = async {
        let mut raw: Vec<u8> = Vec::new();
        let mut out_buf = [0u8; 8192];
        let mut err_buf = [0u8; 8192];
        loop {
            tokio::select! {
                n = stdout_pipe.read(&mut out_buf) => match n {
                    Ok(0) | Err(_) => break,
                    Ok(n) => if raw.len() < READ_CHUNK_BYTES { raw.extend_from_slice(&out_buf[..n]); },
                },
                n = stderr_pipe.read(&mut err_buf) => match n {
                    Ok(0) | Err(_) => continue,
                    Ok(n) => if raw.len() < READ_CHUNK_BYTES { raw.extend_from_slice(&err_buf[..n]); },
                },
            }
        }
        raw
    };

    let deadline = Duration::from_secs(spec.timeout_secs.max(1));
    let joined = tokio::time::timeout(deadline, async {
        let raw = drain.await;
        let status = child.wait().await;
        (raw, status)
    })
    .await;

    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    let (raw, status) = match joined {
        Ok(pair) => pair,
        Err(_) => {
            // Dropping `child` (kill_on_drop) terminates the process.
            drop(child);
            let (output, truncated) = bound_and_redact(b"", spec.output_cap_bytes);
            return ValidatorResult {
                name: spec.name.clone(),
                outcome: ValidatorOutcome::TimedOut,
                exit_code: None,
                duration_ms,
                output,
                output_truncated: truncated,
            };
        }
    };

    match status {
        Ok(status) => {
            let (output, truncated) = bound_and_redact(&raw, spec.output_cap_bytes);
            ValidatorResult {
                name: spec.name.clone(),
                outcome: if status.success() {
                    ValidatorOutcome::Passed
                } else {
                    ValidatorOutcome::Failed
                },
                exit_code: status.code(),
                duration_ms,
                output,
                output_truncated: truncated,
            }
        }
        Err(error) => spawn_error_result(&spec.name, format!("wait failed: {error}")),
    }
}

/// Journal one evidence event, mirroring the agent's recorder failure modes.
async fn journal_event(
    recorder: &RunRecorder,
    kind: &str,
    payload: serde_json::Value,
) -> crate::error::Result<()> {
    use crate::run_journal::RecorderFailureMode;

    if let Err(error) = recorder.record(unix_timestamp_ms(), kind, payload) {
        match recorder.failure_mode() {
            RecorderFailureMode::Warn => {
                warn!(error = %error, kind, "Run recorder failed; continuing in warn mode");
            }
            RecorderFailureMode::Fail => {
                return Err(crate::error::Error::Agent(format!(
                    "run recorder failed: {error}"
                )));
            }
        }
    }
    Ok(())
}

/// Execute a full validation pass against `workspace`.
///
/// Runs validators in declaration order, honoring `fail_fast` (remaining
/// validators are recorded as skipped after the first *required* failure),
/// journals per-validator and summary evidence, and returns the aggregate
/// result. Without a recorder the pass still executes — evidence events are
/// simply omitted.
pub async fn run_validation_pass(
    policy: &ValidationPolicy,
    workspace: &Path,
    recorder: Option<&Arc<RunRecorder>>,
) -> crate::error::Result<ValidationPassResult> {
    let started = Instant::now();

    if let Some(recorder) = recorder {
        journal_event(
            recorder,
            "validation_pass",
            serde_json::json!({
                "phase": "start",
                "policy": policy.to_policy_value(),
                "workspace": workspace.display().to_string(),
            }),
        )
        .await?;
    }

    let mut results: Vec<ValidatorResult> = Vec::with_capacity(policy.validators.len());
    let mut passed = true;
    let mut stopped_reason: Option<String> = None;

    for spec in &policy.validators {
        if stopped_reason.is_some() {
            let result = skipped_result(&spec.name, stopped_reason.as_deref().unwrap_or(""));
            journal_validator(recorder, &result).await?;
            results.push(result);
            continue;
        }

        let result = run_validator(spec, workspace).await;
        info!(
            validator = %spec.name,
            outcome = result.outcome.as_str(),
            exit_code = ?result.exit_code,
            duration_ms = result.duration_ms,
            "Validator finished"
        );

        if let Some(recorder) = recorder {
            journal_event(
                recorder,
                "validator_result",
                serde_json::json!({
                    "name": result.name,
                    "required": spec.required,
                    "outcome": result.outcome.as_str(),
                    "exit_code": result.exit_code,
                    "duration_ms": result.duration_ms,
                    "output": result.output,
                    "output_truncated": result.output_truncated,
                    "provider_kind": recorder.provider_kind().ok(),
                    "model": recorder.model().ok(),
                }),
            )
            .await?;
        }

        if !result.satisfies(spec.required) {
            if spec.required {
                passed = false;
            }
            if policy.fail_fast && spec.required {
                stopped_reason = Some(format!(
                    "required validator '{}' failed (fail_fast)",
                    spec.name
                ));
            }
        }

        results.push(result);
    }

    let pass_result = ValidationPassResult { results, passed };
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    if let Some(recorder) = recorder {
        journal_event(
            recorder,
            "validation_pass",
            serde_json::json!({
                "phase": "end",
                "passed": pass_result.passed,
                "duration_ms": duration_ms,
                "stopped_reason": stopped_reason,
                "results": pass_result.results.iter().map(|r| serde_json::json!({
                    "name": r.name,
                    "outcome": r.outcome.as_str(),
                    "exit_code": r.exit_code,
                    "duration_ms": r.duration_ms,
                    "output_truncated": r.output_truncated,
                })).collect::<Vec<_>>(),
            }),
        )
        .await?;
    }

    Ok(pass_result)
}

async fn journal_validator(
    recorder: Option<&Arc<RunRecorder>>,
    result: &ValidatorResult,
) -> crate::error::Result<()> {
    if let Some(recorder) = recorder {
        journal_event(
            recorder,
            "validator_result",
            serde_json::json!({
                "name": result.name,
                "outcome": result.outcome.as_str(),
                "exit_code": result.exit_code,
                "duration_ms": result.duration_ms,
                "output": result.output,
                "output_truncated": result.output_truncated,
                "skipped_by_fail_fast": true,
            }),
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validation::{ValidationPolicy, DEFAULT_OUTPUT_CAP_BYTES};

    fn spec(name: &str, command: &str) -> ValidatorSpec {
        ValidatorSpec {
            name: name.to_string(),
            command: command.to_string(),
            required: true,
            timeout_secs: DEFAULT_TIMEOUT_TEST,
            workdir: None,
            output_cap_bytes: DEFAULT_OUTPUT_CAP_BYTES,
        }
    }

    const DEFAULT_TIMEOUT_TEST: u64 = 10;

    fn policy(validators: Vec<ValidatorSpec>) -> ValidationPolicy {
        ValidationPolicy {
            enabled: true,
            validators,
            fail_fast: false,
        }
    }

    #[tokio::test]
    async fn passing_validator_reports_passed_with_evidence() {
        let ws = tempfile::tempdir().unwrap();
        let p = policy(vec![spec("true-cmd", "true")]);
        let result = run_validation_pass(&p, ws.path(), None).await.unwrap();
        assert!(result.passed);
        assert_eq!(result.results.len(), 1);
        assert_eq!(result.results[0].outcome, ValidatorOutcome::Passed);
        assert_eq!(result.results[0].exit_code, Some(0));
    }

    #[cfg(windows)]
    fn win_args(extra: &str) -> String {
        format!("cmd /c {extra}")
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failing_required_validator_fails_the_pass() {
        let ws = tempfile::tempdir().unwrap();
        let p = policy(vec![spec("fail-cmd", "false")]);
        let result = run_validation_pass(&p, ws.path(), None).await.unwrap();
        assert!(!result.passed);
        assert_eq!(result.results[0].outcome, ValidatorOutcome::Failed);
        assert_eq!(result.results[0].exit_code, Some(1));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn failing_required_validator_fails_the_pass_windows() {
        let ws = tempfile::tempdir().unwrap();
        let p = policy(vec![spec("fail-cmd", win_args("exit 3"))]);
        let result = run_validation_pass(&p, ws.path(), None).await.unwrap();
        assert!(!result.passed);
        assert_eq!(result.results[0].outcome, ValidatorOutcome::Failed);
        assert_eq!(result.results[0].exit_code, Some(3));
    }

    #[tokio::test]
    async fn unknown_program_maps_to_spawn_error_not_panic() {
        let ws = tempfile::tempdir().unwrap();
        let p = policy(vec![spec("ghost", "definitely-not-a-real-program-kerux")]);
        let result = run_validation_pass(&p, ws.path(), None).await.unwrap();
        assert!(!result.passed);
        assert_eq!(result.results[0].outcome, ValidatorOutcome::SpawnError);
    }

    #[test]
    fn path_like_programs_are_rejected() {
        assert!(program_allowed("cargo"));
        assert!(!program_allowed("./evil.sh"));
        assert!(!program_allowed("/bin/sh"));
        assert!(!program_allowed("..\\evil.exe"));
        assert!(!program_allowed(".."));
        assert!(!program_allowed(""));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn path_program_is_blocked_at_runtime() {
        let ws = tempfile::tempdir().unwrap();
        let script = ws.path().join("evil.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        let p = policy(vec![spec("path-prog", "./evil.sh")]);
        let result = run_validation_pass(&p, ws.path(), None).await.unwrap();
        assert_eq!(result.results[0].outcome, ValidatorOutcome::SpawnError);
        assert!(result.results[0].output.contains("not a bare name"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn escaping_workdir_is_blocked_at_runtime() {
        let ws = tempfile::tempdir().unwrap();
        let mut s = spec("escapee", "true");
        s.workdir = Some("../outside".to_string());
        let p = policy(vec![s]);
        let result = run_validation_pass(&p, ws.path(), None).await.unwrap();
        assert_eq!(result.results[0].outcome, ValidatorOutcome::SpawnError);
        assert!(result.results[0].output.contains("escapes the workspace"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn validator_timeout_is_reported_as_timed_out() {
        let ws = tempfile::tempdir().unwrap();
        let mut s = spec("slow", "sleep 30");
        s.timeout_secs = 1;
        let p = policy(vec![s]);
        let started = std::time::Instant::now();
        let result = run_validation_pass(&p, ws.path(), None).await.unwrap();
        assert_eq!(result.results[0].outcome, ValidatorOutcome::TimedOut);
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn output_is_capped_and_flagged() {
        let ws = tempfile::tempdir().unwrap();
        let mut s = spec("chatty", "head -c 4096 /dev/urandom");
        s.output_cap_bytes = 1024;
        let p = policy(vec![s]);
        let result = run_validation_pass(&p, ws.path(), None).await.unwrap();
        assert_eq!(result.results[0].outcome, ValidatorOutcome::Passed);
        assert!(result.results[0].output_truncated);
        assert!(result.results[0].output.len() <= 2048);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn captured_output_is_redacted() {
        let ws = tempfile::tempdir().unwrap();
        let p = policy(vec![spec("leaky", "echo sk-live-abcdef1234567890abcdef")]);
        let result = run_validation_pass(&p, ws.path(), None).await.unwrap();
        assert!(!result.results[0].output.contains("sk-live-abcdef"));
        assert!(result.results[0].output.contains("[REDACTED]"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fail_fast_marks_remaining_validators_skipped() {
        let ws = tempfile::tempdir().unwrap();
        let mut second = spec("second", "true");
        second.required = false;
        let p = ValidationPolicy {
            enabled: true,
            validators: vec![spec("first-fails", "false"), second],
            fail_fast: true,
        };
        let result = run_validation_pass(&p, ws.path(), None).await.unwrap();
        assert!(!result.passed);
        assert_eq!(result.results[0].outcome, ValidatorOutcome::Failed);
        assert_eq!(result.results[1].outcome, ValidatorOutcome::Skipped);
        assert!(result.results[1].output.contains("fail_fast"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn optional_failure_does_not_fail_the_pass() {
        let ws = tempfile::tempdir().unwrap();
        let mut optional = spec("optional-fails", "false");
        optional.required = false;
        let p = policy(vec![optional, spec("required-ok", "true")]);
        let result = run_validation_pass(&p, ws.path(), None).await.unwrap();
        assert!(result.passed);
        assert_eq!(result.results[0].outcome, ValidatorOutcome::Failed);
        assert_eq!(result.results[1].outcome, ValidatorOutcome::Passed);
    }
}
