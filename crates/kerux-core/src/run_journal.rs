//! Versioned run-journal schema, crash-aware storage, and hash-chain verification.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Current run-journal schema version.
pub const SCHEMA_VERSION: u32 = 1;
const MAX_EVENT_KIND_BYTES: usize = 256;

/// Terminal and non-terminal states stored in a v1 run manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Incomplete,
}

/// How completely a recorded run can be reconstructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Replayability {
    Full,
    Degraded,
    InspectionOnly,
}

/// Version-one metadata for one persisted run journal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunManifestV1 {
    pub schema_version: u32,
    pub run_id: String,
    pub parent_run_id: Option<String>,
    pub parent_sequence: Option<u64>,
    pub created_at_ms: u64,
    pub completed_at_ms: Option<u64>,
    pub status: RunStatus,
    pub surface: String,
    pub model: String,
    pub provider_kind: String,
    pub workspace_fingerprint: String,
    pub repository_head: Option<String>,
    pub repository_dirty_hash: Option<String>,
    pub recorder_policy: Value,
    pub last_sequence: Option<u64>,
    pub last_hash: Option<String>,
    pub replayability: Replayability,
    pub warnings: Vec<String>,
}

/// Errors returned while creating or accessing a persisted run journal.
#[derive(Debug, Error)]
pub enum JournalError {
    #[error("invalid run id: {0}")]
    InvalidRunId(String),
    #[error("invalid run manifest: {0}")]
    InvalidManifest(String),
    #[error("run journal has an incomplete final event")]
    IncompleteTail,
    #[error("run journal already has an active writer")]
    WriterLocked,
    #[error("event line {line} is corrupt: {source}")]
    CorruptEventLine {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("run journal failed verification: {0:?}")]
    Verification(VerificationError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Whether every byte in `events.ndjson` belongs to a complete event line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailState {
    Complete,
    IncompleteTail,
}

/// Whether recorder I/O failures should fail the agent run or only warn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecorderFailureMode {
    Warn,
    Fail,
}

/// Thread-safe owner for one active run journal.
#[derive(Debug)]
pub struct RunRecorder {
    journal: std::sync::Mutex<RunJournal>,
    failure_mode: RecorderFailureMode,
}

impl RunRecorder {
    /// Create a recorder that warns and keeps the agent running on I/O failure.
    pub fn new(journal: RunJournal) -> Self {
        Self::with_failure_mode(journal, RecorderFailureMode::Warn)
    }

    pub fn with_failure_mode(journal: RunJournal, failure_mode: RecorderFailureMode) -> Self {
        Self {
            journal: std::sync::Mutex::new(journal),
            failure_mode,
        }
    }

    pub fn failure_mode(&self) -> RecorderFailureMode {
        self.failure_mode
    }

    pub fn record(
        &self,
        timestamp_ms: u64,
        kind: impl Into<String>,
        payload: Value,
    ) -> Result<(), JournalError> {
        let mut journal = self.lock_journal()?;
        journal.append(timestamp_ms, kind, payload)?;
        Ok(())
    }

    pub fn finalize(&self, status: RunStatus, completed_at_ms: u64) -> Result<(), JournalError> {
        self.lock_journal()?.finalize(status, completed_at_ms)
    }

    /// Append one terminal event and finalize its manifest while holding the
    /// recorder lock for the whole transition.
    pub fn finish(
        &self,
        completed_at_ms: u64,
        kind: impl Into<String>,
        payload: Value,
        status: RunStatus,
    ) -> Result<(), JournalError> {
        let mut journal = self.lock_journal()?;
        journal.append(completed_at_ms, kind, payload)?;
        journal.finalize(status, completed_at_ms)
    }

    pub fn events(&self) -> Result<Vec<RunEventEnvelope>, JournalError> {
        Ok(self.lock_journal()?.events().to_vec())
    }

    fn lock_journal(&self) -> Result<std::sync::MutexGuard<'_, RunJournal>, JournalError> {
        self.journal
            .lock()
            .map_err(|_| JournalError::InvalidManifest("run recorder lock is poisoned".to_string()))
    }
}

/// A filesystem-backed append-only journal.
#[derive(Debug)]
pub struct RunJournal {
    run_dir: PathBuf,
    manifest: RunManifestV1,
    chain: RunChain,
    tail_state: TailState,
    event_file: std::fs::File,
}

impl RunJournal {
    /// Create a run under `$KERUX_HOME/runs/<run_id>`.
    pub fn create(manifest: RunManifestV1) -> Result<Self, JournalError> {
        Self::create_in(crate::platform::kerux_home().join("runs"), manifest)
    }

    /// Create a run under an explicit runs root.
    pub fn create_in(
        runs_root: impl AsRef<Path>,
        manifest: RunManifestV1,
    ) -> Result<Self, JournalError> {
        Self::create_in_with_before_publish(runs_root, manifest, |_| Ok(()))
    }

    fn create_in_with_before_publish(
        runs_root: impl AsRef<Path>,
        manifest: RunManifestV1,
        before_publish: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<Self, JournalError> {
        validate_new_manifest(&manifest)?;
        let manifest = redact_manifest(manifest);

        let runs_root = runs_root.as_ref();
        std::fs::create_dir_all(runs_root)?;
        // Skip permissions on Windows test environments to avoid Access Denied
        #[cfg(not(target_os = "windows"))]
        crate::platform::set_secure_permissions(runs_root)?;

        let run_dir = runs_root.join(&manifest.run_id);
        if std::fs::symlink_metadata(&run_dir).is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("run '{}' already exists", manifest.run_id),
            )
            .into());
        }

        let staging_dir = unique_sibling_path(runs_root, &format!(".{}.tmp", manifest.run_id))?;
        std::fs::create_dir(&staging_dir)?;
        let staged = (|| -> Result<std::fs::File, JournalError> {
            // Skip permissions on Windows test environments to avoid Access Denied
            #[cfg(not(target_os = "windows"))]
            crate::platform::set_secure_permissions(&staging_dir)?;

            let artifacts_dir = staging_dir.join("artifacts");
            std::fs::create_dir(&artifacts_dir)?;
            // Skip permissions on Windows test environments to avoid Access Denied
            #[cfg(not(target_os = "windows"))]
            crate::platform::set_secure_permissions(&artifacts_dir)?;

            write_manifest(&staging_dir.join("manifest.json"), &manifest)?;

            let events_path = staging_dir.join("events.ndjson");
            let event_file = open_new_event_file(&events_path)?;
            // On Windows, an outstanding byte-range lock (LockFileEx) makes
            // the file unrenamable with ERROR_ACCESS_DENIED, so the lock is
            // acquired after the staging directory is published below. On
            // Unix, flock follows the descriptor across rename.
            #[cfg(not(target_os = "windows"))]
            lock_event_file(&event_file)?;
            event_file.sync_all()?;
            sync_directory(&staging_dir)?;
            before_publish(&staging_dir)?;
            Ok(event_file)
        })();
        let event_file = match staged {
            Ok(file) => file,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&staging_dir);
                return Err(error);
            }
        };

        // On Windows, an open handle inside the staging directory makes
        // MoveFileExW fail with ERROR_ACCESS_DENIED, so the event file is
        // closed before the rename and reopened from the published location.
        // On Unix, flock follows the descriptor across rename, so the handle
        // can stay open.
        #[cfg(target_os = "windows")]
        let event_file = {
            drop(event_file);
            if let Err(error) = std::fs::rename(&staging_dir, &run_dir) {
                let _ = std::fs::remove_dir_all(&staging_dir);
                return Err(error.into());
            }
            let events_path = run_dir.join("events.ndjson");
            let event_file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&events_path)?;
            lock_event_file(&event_file)?;
            event_file
        };

        #[cfg(not(target_os = "windows"))]
        {
            if let Err(error) = std::fs::rename(&staging_dir, &run_dir) {
                let _ = std::fs::remove_dir_all(&staging_dir);
                return Err(error.into());
            }
        }
        sync_directory(runs_root)?;

        Ok(Self {
            run_dir,
            chain: RunChain::new(manifest.run_id.clone()),
            manifest,
            tail_state: TailState::Complete,
            event_file,
        })
    }

    /// Reopen and verify an existing run under `$KERUX_HOME/runs`.
    pub fn open(run_id: &str) -> Result<Self, JournalError> {
        Self::open_in(crate::platform::kerux_home().join("runs"), run_id)
    }

    /// Reopen and verify an existing run under an explicit runs root.
    pub fn open_in(runs_root: impl AsRef<Path>, run_id: &str) -> Result<Self, JournalError> {
        validate_run_id(run_id)?;
        let run_dir = runs_root.as_ref().join(run_id);
        if !std::fs::symlink_metadata(&run_dir)?.file_type().is_dir() {
            return Err(JournalError::InvalidManifest(
                "run path must be a real directory".to_string(),
            ));
        }
        let manifest_path = run_dir.join("manifest.json");
        if !std::fs::symlink_metadata(&manifest_path)?
            .file_type()
            .is_file()
        {
            return Err(JournalError::InvalidManifest(
                "manifest.json must be a regular file".to_string(),
            ));
        }
        let mut manifest: RunManifestV1 = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
        validate_existing_manifest(&manifest, run_id)?;

        let events_path = run_dir.join("events.ndjson");
        if !std::fs::symlink_metadata(&events_path)?
            .file_type()
            .is_file()
        {
            return Err(JournalError::InvalidManifest(
                "events.ndjson must be a regular file".to_string(),
            ));
        }
        let mut event_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&events_path)?;
        if !event_file.metadata()?.is_file() {
            return Err(JournalError::InvalidManifest(
                "events.ndjson must be a regular file".to_string(),
            ));
        }
        lock_event_file(&event_file)?;
        let (events, tail_state) = read_events(&mut event_file)?;
        verify_chain_for_run(&events, run_id).map_err(JournalError::Verification)?;
        reconcile_manifest_head(&run_dir, &mut manifest, &events, tail_state)?;

        Ok(Self {
            run_dir,
            manifest,
            chain: RunChain {
                run_id: run_id.to_string(),
                events,
            },
            tail_state,
            event_file,
        })
    }

    /// Append one durable NDJSON event and atomically advance the manifest head.
    pub fn append(
        &mut self,
        timestamp_ms: u64,
        kind: impl Into<String>,
        payload: Value,
    ) -> Result<&RunEventEnvelope, JournalError> {
        if self.tail_state == TailState::IncompleteTail {
            return Err(JournalError::IncompleteTail);
        }
        if self.manifest.status != RunStatus::Running {
            return Err(JournalError::InvalidManifest(
                "cannot append to a finalized run".to_string(),
            ));
        }

        let max_payload_bytes = recorder_max_payload_bytes(&self.manifest)?;
        let bounded = crate::redaction::BoundedPayload::from_json(&payload, max_payload_bytes)?;
        let persisted_payload = serde_json::to_value(bounded)?;
        let persisted_kind = bounded_redacted_text(&kind.into(), MAX_EVENT_KIND_BYTES);
        let event = self
            .chain
            .append(timestamp_ms, persisted_kind, persisted_payload)?
            .clone();
        let mut line = serde_json::to_vec(&event)?;
        line.push(b'\n');
        if let Err(error) = self
            .event_file
            .seek(SeekFrom::End(0))
            .and_then(|_| self.event_file.write_all(&line))
            .and_then(|()| self.event_file.sync_data())
        {
            self.tail_state = TailState::IncompleteTail;
            return Err(error.into());
        }

        self.manifest.last_sequence = Some(event.sequence);
        self.manifest.last_hash = Some(event.hash.clone());
        let manifest_path = self.run_dir.join("manifest.json");
        write_manifest(&manifest_path, &self.manifest)?;
        Ok(self
            .chain
            .events
            .last()
            .expect("the persisted event was just appended"))
    }

    /// Atomically mark a running journal with one terminal status.
    pub fn finalize(
        &mut self,
        status: RunStatus,
        completed_at_ms: u64,
    ) -> Result<(), JournalError> {
        if self.manifest.status != RunStatus::Running {
            return Err(JournalError::InvalidManifest(
                "run is already finalized".to_string(),
            ));
        }
        if status == RunStatus::Running {
            return Err(JournalError::InvalidManifest(
                "final status cannot be running".to_string(),
            ));
        }
        if self.tail_state == TailState::IncompleteTail {
            return Err(JournalError::IncompleteTail);
        }

        self.manifest.status = status;
        self.manifest.completed_at_ms = Some(completed_at_ms);
        write_manifest(&self.run_dir.join("manifest.json"), &self.manifest)
    }

    /// Directory containing this run's manifest, events, and artifacts.
    pub fn run_dir(&self) -> &Path {
        &self.run_dir
    }

    /// Events recovered and verified from complete NDJSON lines.
    pub fn events(&self) -> &[RunEventEnvelope] {
        self.chain.events()
    }

    /// State of the final NDJSON line observed while opening the journal.
    pub fn tail_state(&self) -> TailState {
        self.tail_state
    }
}

fn validate_new_manifest(manifest: &RunManifestV1) -> Result<(), JournalError> {
    validate_run_id(&manifest.run_id)?;
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(JournalError::InvalidManifest(format!(
            "schema version {} is unsupported",
            manifest.schema_version
        )));
    }
    if manifest.status != RunStatus::Running
        || manifest.completed_at_ms.is_some()
        || manifest.last_sequence.is_some()
        || manifest.last_hash.is_some()
    {
        return Err(JournalError::InvalidManifest(
            "new runs must start empty with running status".to_string(),
        ));
    }
    validate_parent_lineage(manifest)?;
    recorder_max_payload_bytes(manifest)?;
    Ok(())
}

fn bounded_redacted_text(value: &str, max_bytes: usize) -> String {
    let redacted = crate::redaction::redact_text(value);
    if redacted.len() <= max_bytes {
        return redacted;
    }
    let mut end = max_bytes;
    while !redacted.is_char_boundary(end) {
        end -= 1;
    }
    redacted[..end].to_string()
}

fn redact_manifest(mut manifest: RunManifestV1) -> RunManifestV1 {
    manifest.surface = crate::redaction::redact_text(&manifest.surface);
    manifest.model = crate::redaction::redact_text(&manifest.model);
    manifest.provider_kind = crate::redaction::redact_text(&manifest.provider_kind);
    manifest.workspace_fingerprint = crate::redaction::redact_text(&manifest.workspace_fingerprint);
    manifest.repository_head = manifest
        .repository_head
        .map(|value| crate::redaction::redact_text(&value));
    manifest.repository_dirty_hash = manifest
        .repository_dirty_hash
        .map(|value| crate::redaction::redact_text(&value));
    manifest.recorder_policy = crate::redaction::redact_json(&manifest.recorder_policy);
    manifest.warnings = manifest
        .warnings
        .into_iter()
        .map(|warning| crate::redaction::redact_text(&warning))
        .collect();
    manifest
}

fn unique_sibling_path(parent: &Path, prefix: &str) -> Result<PathBuf, JournalError> {
    let mut random = [0_u8; 8];
    getrandom::getrandom(&mut random).map_err(|error| {
        std::io::Error::other(format!("failed to generate journal temp name: {error}"))
    })?;
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(parent.join(format!("{prefix}.{suffix}")))
}

fn write_manifest(path: &Path, manifest: &RunManifestV1) -> Result<(), JournalError> {
    let json = serde_json::to_vec_pretty(manifest)?;
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "manifest path has no parent",
        )
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "manifest path has no file name",
        )
    })?;
    let prefix = format!(".{}.tmp", file_name.to_string_lossy());
    let temp_path = unique_sibling_path(parent, &prefix)?;

    let write_result = (|| -> Result<(), JournalError> {
        let mut file = open_new_private_file(&temp_path)?;
        file.write_all(&json)?;
        file.sync_all()?;
        std::fs::rename(&temp_path, path)?;
        sync_directory(parent)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    write_result
}

fn open_new_event_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    // On Unix, append mode is atomic; on Windows, append mode strips
    // FILE_WRITE_DATA from the access mask, which breaks LockFileEx and
    // FlushFileBuffers (sync_all/sync_data) with "Access is denied"
    // (rust-lang/rust#54118). Use plain write mode and seek-to-end before
    // each append instead.
    set_private_creation_mode(&mut options);
    options.open(path)
}

fn open_new_private_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    set_private_creation_mode(&mut options);
    options.open(path)
}

#[cfg(unix)]
fn set_private_creation_mode(options: &mut std::fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(target_os = "windows")]
fn set_private_creation_mode(_options: &mut std::fs::OpenOptions) {}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(target_os = "windows")]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn validate_parent_lineage(manifest: &RunManifestV1) -> Result<(), JournalError> {
    if manifest.parent_run_id.is_some() != manifest.parent_sequence.is_some() {
        return Err(JournalError::InvalidManifest(
            "parent run id and sequence must be present together".to_string(),
        ));
    }
    if let Some(parent_run_id) = &manifest.parent_run_id {
        validate_run_id(parent_run_id)?;
    }
    Ok(())
}

fn recorder_max_payload_bytes(manifest: &RunManifestV1) -> Result<usize, JournalError> {
    let raw = manifest
        .recorder_policy
        .get("max_payload_bytes")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            JournalError::InvalidManifest(
                "recorder_policy.max_payload_bytes must be a positive integer".to_string(),
            )
        })?;
    usize::try_from(raw)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            JournalError::InvalidManifest(
                "recorder_policy.max_payload_bytes is out of range".to_string(),
            )
        })
}

fn validate_run_id(run_id: &str) -> Result<(), JournalError> {
    if run_id.is_empty()
        || crate::redaction::redact_text(run_id) != run_id
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(JournalError::InvalidRunId(run_id.to_string()));
    }
    Ok(())
}

fn validate_existing_manifest(
    manifest: &RunManifestV1,
    expected_run_id: &str,
) -> Result<(), JournalError> {
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(JournalError::InvalidManifest(format!(
            "schema version {} is unsupported",
            manifest.schema_version
        )));
    }
    if manifest.run_id != expected_run_id {
        return Err(JournalError::InvalidManifest(format!(
            "manifest run id '{}' does not match directory '{expected_run_id}'",
            manifest.run_id
        )));
    }
    validate_parent_lineage(manifest)?;
    if manifest.last_sequence.is_some() != manifest.last_hash.is_some() {
        return Err(JournalError::InvalidManifest(
            "last sequence and hash must be present together".to_string(),
        ));
    }
    match manifest.status {
        RunStatus::Running if manifest.completed_at_ms.is_some() => {
            return Err(JournalError::InvalidManifest(
                "running manifest cannot have a completion time".to_string(),
            ));
        }
        RunStatus::Running => {}
        _ if manifest.completed_at_ms.is_none() => {
            return Err(JournalError::InvalidManifest(
                "terminal manifest requires a completion time".to_string(),
            ));
        }
        _ => {}
    }
    recorder_max_payload_bytes(manifest)?;
    Ok(())
}

fn reconcile_manifest_head(
    run_dir: &Path,
    manifest: &mut RunManifestV1,
    events: &[RunEventEnvelope],
    tail_state: TailState,
) -> Result<(), JournalError> {
    let actual_sequence = events.last().map(|event| event.sequence);
    let actual_hash = events.last().map(|event| event.hash.clone());
    if manifest.last_sequence == actual_sequence && manifest.last_hash == actual_hash {
        return Ok(());
    }
    if tail_state == TailState::IncompleteTail
        && manifest.last_sequence
            == actual_sequence.map_or(Some(0), |sequence| sequence.checked_add(1))
        && manifest.last_hash.is_some()
    {
        return Ok(());
    }
    if manifest.status != RunStatus::Running || !manifest_head_is_verified_prefix(manifest, events)
    {
        return Err(JournalError::InvalidManifest(
            "manifest head does not match a verified event prefix".to_string(),
        ));
    }

    manifest.last_sequence = actual_sequence;
    manifest.last_hash = actual_hash;
    write_manifest(&run_dir.join("manifest.json"), manifest)
}

fn manifest_head_is_verified_prefix(manifest: &RunManifestV1, events: &[RunEventEnvelope]) -> bool {
    match (manifest.last_sequence, manifest.last_hash.as_deref()) {
        (None, None) => true,
        (Some(sequence), Some(hash)) => usize::try_from(sequence)
            .ok()
            .and_then(|index| events.get(index))
            .is_some_and(|event| event.sequence == sequence && event.hash == hash),
        _ => false,
    }
}

#[cfg(unix)]
fn lock_event_file(file: &std::fs::File) -> Result<(), JournalError> {
    use std::os::fd::AsRawFd;

    const LOCK_EXCLUSIVE: std::ffi::c_int = 2;
    const LOCK_NONBLOCKING: std::ffi::c_int = 4;

    unsafe extern "C" {
        fn flock(fd: std::ffi::c_int, operation: std::ffi::c_int) -> std::ffi::c_int;
    }

    // SAFETY: `file` owns a valid descriptor for the duration of this call.
    let result = unsafe { flock(file.as_raw_fd(), LOCK_EXCLUSIVE | LOCK_NONBLOCKING) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Err(JournalError::WriterLocked)
    } else {
        Err(error.into())
    }
}

#[cfg(target_os = "windows")]
fn lock_event_file(file: &std::fs::File) -> Result<(), JournalError> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;

    const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x0000_0001;
    const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x0000_0002;
    const ERROR_LOCK_VIOLATION: i32 = 33;

    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: *mut c_void,
    }

    unsafe extern "system" {
        fn LockFileEx(
            file: *mut c_void,
            flags: u32,
            reserved: u32,
            bytes_low: u32,
            bytes_high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
    }

    let mut overlapped = Overlapped {
        internal: 0,
        internal_high: 0,
        offset: 0,
        offset_high: 0,
        event: std::ptr::null_mut(),
    };
    // SAFETY: the handle is valid and `overlapped` remains alive until the
    // non-blocking call completes. Closing the file releases the lock.
    let result = unsafe {
        LockFileEx(
            file.as_raw_handle().cast(),
            LOCKFILE_FAIL_IMMEDIATELY | LOCKFILE_EXCLUSIVE_LOCK,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if result != 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION) {
        Err(JournalError::WriterLocked)
    } else {
        Err(error.into())
    }
}

fn read_events(
    file: &mut std::fs::File,
) -> Result<(Vec<RunEventEnvelope>, TailState), JournalError> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let tail_state = if bytes.is_empty() || bytes.ends_with(b"\n") {
        TailState::Complete
    } else {
        TailState::IncompleteTail
    };
    let complete_bytes = if tail_state == TailState::Complete {
        bytes.as_slice()
    } else {
        let end = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        &bytes[..end]
    };

    let mut events = Vec::new();
    if !complete_bytes.is_empty() {
        let lines = &complete_bytes[..complete_bytes.len() - 1];
        for (index, line) in lines.split(|byte| *byte == b'\n').enumerate() {
            let event =
                serde_json::from_slice(line).map_err(|source| JournalError::CorruptEventLine {
                    line: index + 1,
                    source,
                })?;
            events.push(event);
        }
    }
    Ok((events, tail_state))
}

/// One immutable event in a run journal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunEventEnvelope {
    pub schema_version: u32,
    pub run_id: String,
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub kind: String,
    pub payload: Value,
    pub previous_hash: Option<String>,
    pub hash: String,
}

/// A pure in-memory event-chain builder.
#[derive(Debug, Clone)]
pub struct RunChain {
    run_id: String,
    events: Vec<RunEventEnvelope>,
}

impl RunChain {
    pub fn new(run_id: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            events: Vec::new(),
        }
    }

    pub fn events(&self) -> &[RunEventEnvelope] {
        &self.events
    }

    pub fn append(
        &mut self,
        timestamp_ms: u64,
        kind: impl Into<String>,
        payload: Value,
    ) -> Result<&RunEventEnvelope, serde_json::Error> {
        let mut event = RunEventEnvelope {
            schema_version: SCHEMA_VERSION,
            run_id: self.run_id.clone(),
            sequence: self.events.len() as u64,
            timestamp_ms,
            kind: kind.into(),
            payload,
            previous_hash: self.events.last().map(|event| event.hash.clone()),
            hash: String::new(),
        };
        event.hash = calculate_hash(&event)?;
        self.events.push(event);
        Ok(self.events.last().expect("the event was just appended"))
    }
}

/// Stable reasons why an in-memory chain cannot be verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationError {
    UnsupportedSchemaVersion {
        sequence: u64,
        expected: u32,
        actual: u32,
    },
    SequenceMismatch {
        expected: u64,
        actual: u64,
    },
    RunIdMismatch {
        sequence: u64,
        expected: String,
        actual: String,
    },
    BrokenLink {
        sequence: u64,
    },
    HashMismatch {
        sequence: u64,
    },
}

/// Verify each event's self-hash and link to its predecessor.
pub fn verify_chain(events: &[RunEventEnvelope]) -> Result<(), VerificationError> {
    let Some(expected_run_id) = events.first().map(|event| event.run_id.as_str()) else {
        return Ok(());
    };
    verify_chain_for_run(events, expected_run_id)
}

fn verify_chain_for_run(
    events: &[RunEventEnvelope],
    expected_run_id: &str,
) -> Result<(), VerificationError> {
    for (index, event) in events.iter().enumerate() {
        if event.schema_version != SCHEMA_VERSION {
            return Err(VerificationError::UnsupportedSchemaVersion {
                sequence: event.sequence,
                expected: SCHEMA_VERSION,
                actual: event.schema_version,
            });
        }
        let expected_sequence = index as u64;
        if event.sequence != expected_sequence {
            return Err(VerificationError::SequenceMismatch {
                expected: expected_sequence,
                actual: event.sequence,
            });
        }
        if event.run_id != expected_run_id {
            return Err(VerificationError::RunIdMismatch {
                sequence: event.sequence,
                expected: expected_run_id.to_string(),
                actual: event.run_id.clone(),
            });
        }
        let expected_previous = index
            .checked_sub(1)
            .map(|previous| events[previous].hash.as_str());
        if event.previous_hash.as_deref() != expected_previous {
            return Err(VerificationError::BrokenLink {
                sequence: event.sequence,
            });
        }
        let actual = calculate_hash(event).map_err(|_| VerificationError::HashMismatch {
            sequence: event.sequence,
        })?;
        if actual != event.hash {
            return Err(VerificationError::HashMismatch {
                sequence: event.sequence,
            });
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct HashMaterial<'a> {
    schema_version: u32,
    run_id: &'a str,
    sequence: u64,
    timestamp_ms: u64,
    kind: &'a str,
    payload: &'a Value,
    previous_hash: &'a Option<String>,
}

fn calculate_hash(event: &RunEventEnvelope) -> Result<String, serde_json::Error> {
    let material = HashMaterial {
        schema_version: event.schema_version,
        run_id: &event.run_id,
        sequence: event.sequence,
        timestamp_ms: event.timestamp_ms,
        kind: &event.kind,
        payload: &event.payload,
        previous_hash: &event.previous_hash,
    };
    let bytes = serde_json::to_vec(&material)?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    fn test_manifest(run_id: &str) -> super::RunManifestV1 {
        super::RunManifestV1 {
            schema_version: super::SCHEMA_VERSION,
            run_id: run_id.to_string(),
            parent_run_id: None,
            parent_sequence: None,
            created_at_ms: 1_725_000_000_000,
            completed_at_ms: None,
            status: super::RunStatus::Running,
            surface: "cli".to_string(),
            model: "test-model".to_string(),
            provider_kind: "test-provider".to_string(),
            workspace_fingerprint: "workspace-sha256".to_string(),
            repository_head: None,
            repository_dirty_hash: None,
            recorder_policy: json!({"max_payload_bytes": 1024}),
            last_sequence: None,
            last_hash: None,
            replayability: super::Replayability::Full,
            warnings: Vec::new(),
        }
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    #[serial_test::serial]
    fn empty_kerux_home_falls_back_to_platform_home() {
        let home = tempfile::tempdir().unwrap();
        let previous_kerux_home = std::env::var_os("KERUX_HOME");
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("KERUX_HOME", "");
        std::env::set_var("HOME", home.path());

        let journal = super::RunJournal::create(test_manifest("run-empty-home")).unwrap();

        assert_eq!(
            journal.run_dir(),
            home.path().join(".kerux/runs/run-empty-home")
        );
        match previous_kerux_home {
            Some(value) => std::env::set_var("KERUX_HOME", value),
            None => std::env::remove_var("KERUX_HOME"),
        }
        match previous_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn kerux_home_override_isolates_new_run_storage() {
        let home = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("KERUX_HOME");
        std::env::set_var("KERUX_HOME", home.path());

        let journal = super::RunJournal::create(test_manifest("run-home-override")).unwrap();
        drop(journal);
        let journal = super::RunJournal::open("run-home-override").unwrap();

        assert_eq!(
            journal.run_dir(),
            home.path().join("runs").join("run-home-override")
        );
        assert!(journal.run_dir().join("manifest.json").is_file());
        assert!(journal.run_dir().join("events.ndjson").is_file());
        assert!(journal.run_dir().join("artifacts").is_dir());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let dir_mode = std::fs::metadata(journal.run_dir())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let manifest_mode = std::fs::metadata(journal.run_dir().join("manifest.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let events_mode = std::fs::metadata(journal.run_dir().join("events.ndjson"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700);
            assert_eq!(manifest_mode, 0o600);
            assert_eq!(events_mode, 0o600);
        }

        match previous {
            Some(value) => std::env::set_var("KERUX_HOME", value),
            None => std::env::remove_var("KERUX_HOME"),
        }
    }

    #[test]
    fn failed_creation_before_publication_does_not_consume_the_run_id() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let manifest = test_manifest("run-retry-create");

        let error =
            super::RunJournal::create_in_with_before_publish(&runs_root, manifest.clone(), |_| {
                Err(std::io::Error::other("injected pre-publication failure"))
            })
            .unwrap_err();

        assert!(matches!(error, super::JournalError::Io(_)));
        assert!(!runs_root.join("run-retry-create").exists());
        assert!(std::fs::read_dir(&runs_root).unwrap().next().is_none());
        let journal = super::RunJournal::create_in(&runs_root, manifest).unwrap();
        assert_eq!(journal.run_dir(), runs_root.join("run-retry-create"));
    }

    #[test]
    fn concurrent_creation_allows_exactly_one_publisher() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = std::sync::Arc::new(home.path().join("runs"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let runs_root = std::sync::Arc::clone(&runs_root);
            let barrier = std::sync::Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                super::RunJournal::create_in(runs_root.as_path(), test_manifest("run-create-race"))
            }));
        }

        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        let reopened = super::RunJournal::open_in(runs_root.as_path(), "run-create-race");
        assert!(matches!(reopened, Err(super::JournalError::WriterLocked)));
    }

    #[test]
    #[cfg(unix)]
    fn reopen_rejects_a_symlinked_run_directory() {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let real_root = home.path().join("real-runs");
        let journal =
            super::RunJournal::create_in(&real_root, test_manifest("run-symlinked")).unwrap();
        drop(journal);
        std::fs::create_dir_all(&runs_root).unwrap();
        symlink(
            real_root.join("run-symlinked"),
            runs_root.join("run-symlinked"),
        )
        .unwrap();

        let error = super::RunJournal::open_in(&runs_root, "run-symlinked").unwrap_err();

        assert!(matches!(error, super::JournalError::InvalidManifest(_)));
    }

    #[test]
    fn second_writer_is_rejected_while_a_run_is_open() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let _first =
            super::RunJournal::create_in(&runs_root, test_manifest("run-single-writer")).unwrap();

        let error = super::RunJournal::open_in(&runs_root, "run-single-writer").unwrap_err();

        assert!(matches!(error, super::JournalError::WriterLocked));
    }

    #[test]
    fn append_then_reopen_preserves_the_next_sequence_and_hash() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let mut journal =
            super::RunJournal::create_in(&runs_root, test_manifest("run-resume")).unwrap();
        let first_hash = journal
            .append(1_725_000_000_001, "run_started", json!({"surface": "cli"}))
            .unwrap()
            .hash
            .clone();
        drop(journal);

        let mut reopened = super::RunJournal::open_in(&runs_root, "run-resume").unwrap();
        assert_eq!(reopened.tail_state(), super::TailState::Complete);
        let second = reopened
            .append(1_725_000_000_002, "request_prepared", json!({}))
            .unwrap();

        assert_eq!(second.sequence, 1);
        assert_eq!(second.previous_hash.as_deref(), Some(first_hash.as_str()));
        assert_eq!(super::verify_chain(reopened.events()), Ok(()));
    }

    #[test]
    fn truncated_final_line_is_reported_without_losing_verified_events() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let mut journal =
            super::RunJournal::create_in(&runs_root, test_manifest("run-truncated")).unwrap();
        journal
            .append(1_725_000_000_001, "run_started", json!({}))
            .unwrap();
        drop(journal);
        let events_path = runs_root.join("run-truncated").join("events.ndjson");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&events_path)
            .unwrap();
        std::io::Write::write_all(&mut file, br#"{"schema_version":1"#).unwrap();
        drop(file);

        let reopened = super::RunJournal::open_in(&runs_root, "run-truncated").unwrap();

        assert_eq!(reopened.tail_state(), super::TailState::IncompleteTail);
        assert_eq!(reopened.events().len(), 1);
        assert_eq!(super::verify_chain(reopened.events()), Ok(()));
    }

    #[test]
    fn truncating_a_committed_final_event_preserves_the_verified_prefix() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let mut journal =
            super::RunJournal::create_in(&runs_root, test_manifest("run-truncated-commit"))
                .unwrap();
        journal
            .append(1_725_000_000_001, "run_started", json!({}))
            .unwrap();
        journal
            .append(1_725_000_000_002, "request_prepared", json!({}))
            .unwrap();
        drop(journal);
        let events_path = runs_root.join("run-truncated-commit").join("events.ndjson");
        let raw = std::fs::read(&events_path).unwrap();
        let final_line_start = raw[..raw.len() - 1]
            .iter()
            .rposition(|byte| *byte == b'\n')
            .unwrap()
            + 1;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&events_path)
            .unwrap()
            .set_len((final_line_start + 8) as u64)
            .unwrap();

        let reopened = super::RunJournal::open_in(&runs_root, "run-truncated-commit").unwrap();

        assert_eq!(reopened.tail_state(), super::TailState::IncompleteTail);
        assert_eq!(reopened.events().len(), 1);
        assert_eq!(super::verify_chain(reopened.events()), Ok(()));
    }

    #[test]
    fn persisted_manifest_fields_are_redacted_before_publication() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let secret = "Bearer manifest-secret";
        let mut manifest = test_manifest("run-redacted-manifest");
        manifest.model = secret.to_string();
        manifest.warnings.push(secret.to_string());
        manifest.recorder_policy = json!({
            "max_payload_bytes": 1024,
            "authorization": secret,
        });

        let journal = super::RunJournal::create_in(&runs_root, manifest).unwrap();
        drop(journal);

        let raw = std::fs::read_to_string(
            runs_root
                .join("run-redacted-manifest")
                .join("manifest.json"),
        )
        .unwrap();
        assert!(!raw.contains("manifest-secret"));
        assert!(raw.contains(crate::redaction::REDACTED));
    }

    #[test]
    fn persisted_event_kind_is_redacted_and_bounded_before_hashing() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let mut journal =
            super::RunJournal::create_in(&runs_root, test_manifest("run-kind-redacted")).unwrap();
        let secret = "Bearer event-kind-secret";

        let event = journal
            .append(
                1_725_000_000_001,
                format!("{secret}{}", "x".repeat(300)),
                json!({}),
            )
            .unwrap();

        assert!(!event.kind.contains("event-kind-secret"));
        assert!(event.kind.len() <= super::MAX_EVENT_KIND_BYTES);
        assert_eq!(super::verify_chain(journal.events()), Ok(()));
    }

    #[test]
    fn persisted_payload_is_redacted_and_bounded_before_hashing() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let mut manifest = test_manifest("run-redacted");
        manifest.recorder_policy = json!({"max_payload_bytes": 48});
        let mut journal = super::RunJournal::create_in(&runs_root, manifest).unwrap();
        let secret = "sk-1234567890abcdef";

        journal
            .append(
                1_725_000_000_001,
                "tool_started",
                json!({"authorization": secret, "output": "x".repeat(200)}),
            )
            .unwrap();
        drop(journal);

        let raw =
            std::fs::read_to_string(runs_root.join("run-redacted").join("events.ndjson")).unwrap();
        assert!(!raw.contains(secret));
        let reopened = super::RunJournal::open_in(&runs_root, "run-redacted").unwrap();
        let bounded: crate::redaction::BoundedPayload =
            serde_json::from_value(reopened.events()[0].payload.clone()).unwrap();
        assert!(bounded.truncated);
        assert!(bounded.content.len() <= 48);
        assert!(!bounded.content.contains(secret));
    }

    #[test]
    fn manifest_write_failure_keeps_the_durable_event_in_the_chain() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let mut journal =
            super::RunJournal::create_in(&runs_root, test_manifest("run-manifest-failure"))
                .unwrap();
        let manifest_path = runs_root.join("run-manifest-failure").join("manifest.json");
        let original_manifest = std::fs::read(&manifest_path).unwrap();
        std::fs::remove_file(&manifest_path).unwrap();
        std::fs::create_dir(&manifest_path).unwrap();

        assert!(matches!(
            journal.append(1_725_000_000_001, "run_started", json!({})),
            Err(super::JournalError::Io(_))
        ));
        std::fs::remove_dir(&manifest_path).unwrap();
        std::fs::write(&manifest_path, original_manifest).unwrap();

        let second = journal
            .append(1_725_000_000_002, "request_prepared", json!({}))
            .unwrap();
        assert_eq!(second.sequence, 1);
        drop(journal);

        let reopened = super::RunJournal::open_in(&runs_root, "run-manifest-failure").unwrap();
        assert_eq!(reopened.events().len(), 2);
        assert_eq!(super::verify_chain(reopened.events()), Ok(()));
    }

    #[test]
    fn reopen_repairs_a_running_manifest_that_lags_verified_events() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let mut journal =
            super::RunJournal::create_in(&runs_root, test_manifest("run-recover-head")).unwrap();
        let event_hash = journal
            .append(1_725_000_000_001, "run_started", json!({}))
            .unwrap()
            .hash
            .clone();
        drop(journal);
        let manifest_path = runs_root.join("run-recover-head").join("manifest.json");
        let mut lagging: super::RunManifestV1 =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        lagging.last_sequence = None;
        lagging.last_hash = None;
        crate::persist::write_json(&manifest_path, &lagging).unwrap();

        super::RunJournal::open_in(&runs_root, "run-recover-head").unwrap();

        let repaired: super::RunManifestV1 =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(repaired.last_sequence, Some(0));
        assert_eq!(repaired.last_hash, Some(event_hash));
    }

    #[test]
    fn reopen_rejects_a_complete_cross_run_stream_transplant() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let journal_a = super::RunJournal::create_in(&runs_root, test_manifest("run-a")).unwrap();
        drop(journal_a);
        let mut journal_b =
            super::RunJournal::create_in(&runs_root, test_manifest("run-b")).unwrap();
        journal_b
            .append(1_725_000_000_001, "run_started", json!({}))
            .unwrap();
        drop(journal_b);
        std::fs::copy(
            runs_root.join("run-b").join("events.ndjson"),
            runs_root.join("run-a").join("events.ndjson"),
        )
        .unwrap();

        let error = super::RunJournal::open_in(&runs_root, "run-a").unwrap_err();

        assert!(matches!(
            error,
            super::JournalError::Verification(super::VerificationError::RunIdMismatch {
                sequence: 0,
                ..
            })
        ));
    }

    #[test]
    fn reopen_rejects_a_running_manifest_with_a_forged_head() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let mut journal =
            super::RunJournal::create_in(&runs_root, test_manifest("run-forged-head")).unwrap();
        journal
            .append(1_725_000_000_001, "run_started", json!({}))
            .unwrap();
        drop(journal);
        let manifest_path = runs_root.join("run-forged-head").join("manifest.json");
        let mut forged: super::RunManifestV1 =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        forged.last_hash = Some("0".repeat(64));
        crate::persist::write_json(&manifest_path, &forged).unwrap();

        let error = super::RunJournal::open_in(&runs_root, "run-forged-head").unwrap_err();

        assert!(matches!(error, super::JournalError::InvalidManifest(_)));
    }

    #[test]
    fn finalization_atomically_updates_terminal_manifest_status() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let mut journal =
            super::RunJournal::create_in(&runs_root, test_manifest("run-finalized")).unwrap();
        journal
            .append(1_725_000_000_001, "run_started", json!({}))
            .unwrap();

        journal
            .finalize(super::RunStatus::Succeeded, 1_725_000_000_002)
            .unwrap();

        let manifest: super::RunManifestV1 = serde_json::from_slice(
            &std::fs::read(runs_root.join("run-finalized").join("manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.status, super::RunStatus::Succeeded);
        assert_eq!(manifest.completed_at_ms, Some(1_725_000_000_002));
        assert_eq!(manifest.last_sequence, Some(0));
        assert_eq!(manifest.last_hash, Some(journal.events()[0].hash.clone()));
        assert!(matches!(
            journal.finalize(super::RunStatus::Failed, 1_725_000_000_003),
            Err(super::JournalError::InvalidManifest(_))
        ));
        assert!(matches!(
            journal.append(1_725_000_000_003, "late_event", json!({})),
            Err(super::JournalError::InvalidManifest(_))
        ));
        drop(journal);
        let reopened = super::RunJournal::open_in(&runs_root, "run-finalized").unwrap();
        assert_eq!(reopened.events().len(), 1);
    }

    #[test]
    fn empty_completed_line_is_reported_as_corruption() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let mut journal =
            super::RunJournal::create_in(&runs_root, test_manifest("run-corrupt")).unwrap();
        journal
            .append(1_725_000_000_001, "run_started", json!({}))
            .unwrap();
        drop(journal);
        let events_path = runs_root.join("run-corrupt").join("events.ndjson");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&events_path)
            .unwrap();
        std::io::Write::write_all(&mut file, b"\n").unwrap();
        drop(file);

        let error = super::RunJournal::open_in(&runs_root, "run-corrupt").unwrap_err();

        assert!(matches!(
            error,
            super::JournalError::CorruptEventLine { line: 2, .. }
        ));
    }

    #[test]
    fn completed_event_content_corruption_fails_hash_verification() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let mut journal =
            super::RunJournal::create_in(&runs_root, test_manifest("run-hash-corrupt")).unwrap();
        journal
            .append(1_725_000_000_001, "run_started", json!({"value": 1}))
            .unwrap();
        drop(journal);
        let events_path = runs_root.join("run-hash-corrupt").join("events.ndjson");
        let raw = std::fs::read_to_string(&events_path).unwrap();
        std::fs::write(&events_path, raw.replace("run_started", "run_stopped")).unwrap();

        let error = super::RunJournal::open_in(&runs_root, "run-hash-corrupt").unwrap_err();

        assert!(matches!(
            error,
            super::JournalError::Verification(super::VerificationError::HashMismatch {
                sequence: 0
            })
        ));
    }

    #[test]
    fn new_manifest_rejects_incomplete_parent_lineage() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let mut manifest = test_manifest("run-invalid-parent");
        manifest.parent_run_id = Some("parent-run".to_string());

        let error = super::RunJournal::create_in(&runs_root, manifest).unwrap_err();

        assert!(matches!(error, super::JournalError::InvalidManifest(_)));
        assert!(!runs_root.join("run-invalid-parent").exists());
    }

    #[test]
    fn credential_shaped_run_ids_are_rejected_before_persistence() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let manifest = test_manifest("sk-secretcredential1234567890");

        let error = super::RunJournal::create_in(&runs_root, manifest).unwrap_err();

        assert!(matches!(error, super::JournalError::InvalidRunId(_)));
        assert!(!runs_root.exists());
    }

    #[test]
    fn unsafe_run_ids_cannot_escape_the_runs_root() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");

        let error =
            super::RunJournal::create_in(&runs_root, test_manifest("../escape")).unwrap_err();

        assert!(matches!(error, super::JournalError::InvalidRunId(_)));
        assert!(!home.path().join("escape").exists());
    }

    #[test]
    fn first_event_starts_a_verifiable_chain() {
        let mut chain = super::RunChain::new("run-1");

        let event = chain
            .append(1_725_000_000_000, "run_started", json!({"surface": "cli"}))
            .unwrap();

        let _: u64 = event.timestamp_ms;
        assert_eq!(event.schema_version, 1);
        assert_eq!(event.run_id, "run-1");
        assert_eq!(event.sequence, 0);
        assert_eq!(event.previous_hash, None);
        assert_eq!(event.hash.len(), 64);
        assert_eq!(super::verify_chain(chain.events()), Ok(()));
    }

    #[test]
    fn second_event_links_to_the_first_hash() {
        let mut chain = super::RunChain::new("run-1");
        let first_hash = chain
            .append(1_725_000_000_000, "run_started", json!({}))
            .unwrap()
            .hash
            .clone();

        let second = chain
            .append(1_725_000_000_001, "request_prepared", json!({}))
            .unwrap();

        assert_eq!(second.sequence, 1);
        assert_eq!(second.previous_hash.as_deref(), Some(first_hash.as_str()));
        assert_eq!(super::verify_chain(chain.events()), Ok(()));
    }

    #[test]
    fn payload_mutation_fails_verification() {
        let mut chain = super::RunChain::new("run-1");
        chain
            .append(1_725_000_000_000, "tool_completed", json!({"exit_code": 0}))
            .unwrap();
        let mut events = chain.events().to_vec();
        events[0].payload["exit_code"] = json!(1);

        assert_eq!(
            super::verify_chain(&events),
            Err(super::VerificationError::HashMismatch { sequence: 0 })
        );
    }

    #[test]
    fn sequence_gap_fails_even_when_the_event_hash_was_recomputed() {
        let mut chain = super::RunChain::new("run-1");
        chain.append(1, "run_started", json!({})).unwrap();
        chain.append(2, "run_completed", json!({})).unwrap();
        let mut events = chain.events().to_vec();
        events[1].sequence = 2;
        events[1].hash = super::calculate_hash(&events[1]).unwrap();

        assert_eq!(
            super::verify_chain(&events),
            Err(super::VerificationError::SequenceMismatch {
                expected: 1,
                actual: 2,
            })
        );
    }

    #[test]
    fn cross_run_event_fails_even_when_the_event_hash_was_recomputed() {
        let mut chain = super::RunChain::new("run-1");
        chain.append(1, "run_started", json!({})).unwrap();
        chain.append(2, "run_completed", json!({})).unwrap();
        let mut events = chain.events().to_vec();
        events[1].run_id = "run-2".to_string();
        events[1].hash = super::calculate_hash(&events[1]).unwrap();

        assert_eq!(
            super::verify_chain(&events),
            Err(super::VerificationError::RunIdMismatch {
                sequence: 1,
                expected: "run-1".to_string(),
                actual: "run-2".to_string(),
            })
        );
    }

    #[test]
    fn unsupported_schema_fails_at_any_sequence_with_recomputed_hashes() {
        let mut chain = super::RunChain::new("run-1");
        chain.append(1, "run_started", json!({})).unwrap();
        chain.append(2, "run_completed", json!({})).unwrap();

        for sequence in 0..2 {
            let mut events = chain.events().to_vec();
            events[sequence].schema_version = 2;
            events[sequence].hash = super::calculate_hash(&events[sequence]).unwrap();
            if sequence == 0 {
                events[1].previous_hash = Some(events[0].hash.clone());
                events[1].hash = super::calculate_hash(&events[1]).unwrap();
            }

            assert_eq!(
                super::verify_chain(&events),
                Err(super::VerificationError::UnsupportedSchemaVersion {
                    sequence: sequence as u64,
                    expected: super::SCHEMA_VERSION,
                    actual: 2,
                })
            );
        }
    }

    #[test]
    fn unknown_event_kind_roundtrips_as_raw_json() {
        let mut chain = super::RunChain::new("run-1");
        chain
            .append(
                1_725_000_000_000,
                "future_event_kind",
                json!({"future": {"value": 7}}),
            )
            .unwrap();

        let encoded = serde_json::to_string(chain.events()).unwrap();
        let decoded: Vec<super::RunEventEnvelope> = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded[0].kind, "future_event_kind");
        assert_eq!(decoded[0].payload, json!({"future": {"value": 7}}));
        assert_eq!(super::verify_chain(&decoded), Ok(()));
    }
}
