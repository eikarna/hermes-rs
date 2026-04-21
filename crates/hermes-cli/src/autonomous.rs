use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use hermes_core::config::AppConfig;
use hermes_core::platform::detect_shell;
use tokio::fs;
use tokio::time::{self, MissedTickBehavior};
use tracing::{error, info, warn};

use crate::create_agent_without_events;
use hermes_core::mcp::McpManager;

#[derive(Debug, Clone)]
pub struct WorkspaceSnapshot {
    pub repo_root: PathBuf,
    pub todo_path: PathBuf,
    pub todo_contents: String,
    pub branch: String,
    pub head: String,
    pub worktree_status: String,
    pub implemented_items: Vec<String>,
    pub pending_items: Vec<String>,
}

impl WorkspaceSnapshot {
    fn workspace_key(&self) -> u64 {
        hash_parts([
            self.todo_contents.as_str(),
            self.head.as_str(),
            self.worktree_status.as_str(),
        ])
    }

    fn state_fingerprint(&self, failure_signature: Option<&str>) -> u64 {
        hash_parts([
            self.todo_contents.as_str(),
            self.head.as_str(),
            self.worktree_status.as_str(),
            failure_signature.unwrap_or(""),
        ])
    }
}

#[derive(Debug, Clone)]
pub struct FailureRecord {
    pub workspace_key: u64,
    pub state_fingerprint: u64,
    pub attempts: usize,
    pub last_failure_signature: String,
    pub last_error: String,
    pub paused: bool,
}

#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub description: String,
    pub cwd: PathBuf,
    pub timeout: Duration,
    pub kind: CommandKind,
}

#[derive(Debug, Clone)]
pub enum CommandKind {
    Exec { program: String, args: Vec<String> },
    Shell { command: String },
}

#[derive(Debug, Clone)]
pub struct CommandOutcome {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl CommandOutcome {
    fn failure_signature(&self, fallback: &str) -> String {
        if self.timed_out {
            return format!("{} timed out", fallback);
        }

        for line in self.stderr.lines().chain(self.stdout.lines()) {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }

        match self.exit_code {
            Some(code) => format!("{} failed with exit code {}", fallback, code),
            None => fallback.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
struct TodoLedger {
    implemented: Vec<String>,
    pending: Vec<String>,
}

impl TodoLedger {
    fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

#[async_trait]
trait CommandExecutor: Send + Sync {
    async fn run(&self, spec: CommandSpec) -> Result<CommandOutcome>;
}

#[derive(Debug, Clone, Copy, Default)]
struct RealCommandExecutor;

#[async_trait]
impl CommandExecutor for RealCommandExecutor {
    async fn run(&self, spec: CommandSpec) -> Result<CommandOutcome> {
        run_blocking_command(spec).await
    }
}

pub async fn run_autonomous(config: AppConfig, system_prompt: Option<String>) -> Result<()> {
    let repo_root = std::env::current_dir().context("Failed to determine current directory")?;
    let mut runner = AutonomousRunner::new(config, system_prompt, repo_root, RealCommandExecutor);
    runner.run_loop().await
}

struct AutonomousRunner<E> {
    config: AppConfig,
    system_prompt: Option<String>,
    repo_root: PathBuf,
    executor: E,
    failure: Option<FailureRecord>,
}

impl<E> AutonomousRunner<E>
where
    E: CommandExecutor,
{
    fn new(
        config: AppConfig,
        system_prompt: Option<String>,
        repo_root: PathBuf,
        executor: E,
    ) -> Self {
        Self {
            config,
            system_prompt,
            repo_root,
            executor,
            failure: None,
        }
    }

    async fn run_loop(&mut self) -> Result<()> {
        info!(
            interval_secs = self.config.autonomous.interval_secs,
            todo = %self.todo_path().display(),
            "Starting autonomous mode",
        );

        let mut interval =
            time::interval(Duration::from_secs(self.config.autonomous.interval_secs));
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            interval.tick().await;
            if let Err(error) = self.tick().await {
                error!(error = %error, "Autonomous tick failed");
            }
        }
    }

    async fn tick(&mut self) -> Result<()> {
        let snapshot = match self.capture_workspace_snapshot().await? {
            Some(snapshot) => snapshot,
            None => return Ok(()),
        };

        if !snapshot.pending_items.is_empty() {
            info!(
                pending = snapshot.pending_items.len(),
                branch = %snapshot.branch,
                "Autonomous tick inspecting pending work",
            );
        }

        let prior_failure = self
            .failure
            .as_ref()
            .filter(|record| record.workspace_key == snapshot.workspace_key());
        let current_fingerprint = snapshot
            .state_fingerprint(prior_failure.map(|record| record.last_failure_signature.as_str()));

        if prior_failure
            .is_some_and(|record| record.paused && record.state_fingerprint == current_fingerprint)
        {
            warn!(
                attempts = prior_failure
                    .map(|record| record.attempts)
                    .unwrap_or_default(),
                "Autonomous state is paused until TODO.md or git state changes",
            );
            return Ok(());
        }

        let query = build_autonomous_query(&snapshot, prior_failure);
        let previous_status = snapshot.worktree_status.clone();

        let mut mcp_manager = McpManager::new();
        let agent = create_agent_without_events(
            &self.config,
            self.system_prompt.as_deref(),
            &mut mcp_manager,
        )
        .await?;

        match agent.run(query).await {
            Ok(_) => {}
            Err(error) => {
                let error_text = format!("Agent run failed: {}", error);
                self.record_failure(&snapshot, "agent run failed", error_text);
                return Ok(());
            }
        }

        let current_status = self.git_status().await?;
        if current_status == previous_status {
            self.record_failure(
                &snapshot,
                "no workspace changes",
                "Agent produced no workspace changes.".to_string(),
            );
            return Ok(());
        }

        let post_snapshot = self
            .refresh_snapshot_after_run(&snapshot, current_status)
            .await?;

        let validation = self.run_validation().await?;
        if !validation.success {
            let failure_signature = validation.failure_signature("validation");
            let message = format_command_failure("Validation failed", &validation);
            self.record_failure(&post_snapshot, &failure_signature, message);
            return Ok(());
        }

        self.publish_changes(&post_snapshot).await?;
        self.failure = None;
        info!("Autonomous iteration completed and pushed successfully");
        Ok(())
    }

    async fn capture_workspace_snapshot(&self) -> Result<Option<WorkspaceSnapshot>> {
        let todo_path = self.todo_path();
        let snapshot = match self.refresh_snapshot(todo_path.clone()).await {
            Ok(snapshot) => snapshot,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|io_error| io_error.kind() == std::io::ErrorKind::NotFound) =>
            {
                warn!(path = %todo_path.display(), "TODO.md not found; skipping autonomous tick");
                return Ok(None);
            }
            Err(error) => return Err(error),
        };

        if snapshot.pending_items.is_empty() {
            info!(path = %todo_path.display(), "TODO.md has no pending items; skipping autonomous tick");
            return Ok(None);
        }

        Ok(Some(snapshot))
    }

    async fn refresh_snapshot(&self, todo_path: PathBuf) -> Result<WorkspaceSnapshot> {
        let todo_contents = fs::read_to_string(&todo_path)
            .await
            .with_context(|| format!("Failed to read TODO file '{}'", todo_path.display()))?;
        let ledger = parse_todo_ledger(&todo_contents);
        if !ledger.has_pending() {
            // The caller can decide whether an empty pending list means skip or not.
        }

        let branch = self
            .require_success(
                self.run_command(exec_git(
                    "read git branch",
                    &self.repo_root,
                    &["rev-parse", "--abbrev-ref", "HEAD"],
                    self.command_timeout(),
                ))
                .await,
            )?
            .stdout
            .trim()
            .to_string();
        let head = self
            .require_success(
                self.run_command(exec_git(
                    "read git head",
                    &self.repo_root,
                    &["rev-parse", "HEAD"],
                    self.command_timeout(),
                ))
                .await,
            )?
            .stdout
            .trim()
            .to_string();
        let worktree_status = self.git_status().await?;

        Ok(WorkspaceSnapshot {
            repo_root: self.repo_root.clone(),
            todo_path,
            todo_contents,
            branch,
            head,
            worktree_status,
            implemented_items: ledger.implemented,
            pending_items: ledger.pending,
        })
    }

    fn todo_path(&self) -> PathBuf {
        let configured = &self.config.autonomous.todo_path;
        if configured.is_absolute() {
            configured.clone()
        } else {
            self.repo_root.join(configured)
        }
    }

    fn command_timeout(&self) -> Duration {
        Duration::from_secs(self.config.autonomous.command_timeout_secs)
    }

    async fn git_status(&self) -> Result<String> {
        Ok(self
            .require_success(
                self.run_command(exec_git(
                    "read git status",
                    &self.repo_root,
                    &["status", "--short"],
                    self.command_timeout(),
                ))
                .await,
            )?
            .stdout)
    }

    async fn refresh_snapshot_after_run(
        &self,
        baseline: &WorkspaceSnapshot,
        worktree_status: String,
    ) -> Result<WorkspaceSnapshot> {
        let mut refreshed = self.refresh_snapshot(baseline.todo_path.clone()).await?;
        refreshed.worktree_status = worktree_status;
        refreshed.branch = baseline.branch.clone();
        refreshed.head = baseline.head.clone();
        Ok(refreshed)
    }

    async fn run_validation(&self) -> Result<CommandOutcome> {
        self.run_command(CommandSpec {
            description: "run validation".to_string(),
            cwd: self.repo_root.clone(),
            timeout: self.command_timeout(),
            kind: CommandKind::Shell {
                command: self.config.autonomous.test_command.clone(),
            },
        })
        .await
    }

    async fn publish_changes(&mut self, snapshot: &WorkspaceSnapshot) -> Result<()> {
        let add = self
            .run_command(exec_git(
                "git add",
                &self.repo_root,
                &["add", "."],
                self.command_timeout(),
            ))
            .await?;
        if !add.success {
            return self.fail_publish(snapshot, "git add failed", add);
        }

        let commit = self
            .run_command(exec_git_owned(
                "git commit",
                &self.repo_root,
                vec![
                    "commit".to_string(),
                    "-m".to_string(),
                    self.config.autonomous.commit_message.clone(),
                ],
                self.command_timeout(),
            ))
            .await?;
        if !commit.success {
            if looks_like_nothing_to_commit(&commit) {
                info!("Validation passed but git commit reported nothing to commit; skipping push");
                return Ok(());
            }
            return self.fail_publish(snapshot, "git commit failed", commit);
        }

        let committed_snapshot = self.refresh_snapshot(snapshot.todo_path.clone()).await?;

        let push = self
            .run_command(exec_git_owned(
                "git push",
                &self.repo_root,
                vec![
                    "push".to_string(),
                    self.config.autonomous.git_remote.clone(),
                    self.config.autonomous.git_branch.clone(),
                ],
                self.command_timeout(),
            ))
            .await?;
        if !push.success {
            return self.fail_publish(&committed_snapshot, "git push failed", push);
        }

        Ok(())
    }

    fn fail_publish(
        &mut self,
        snapshot: &WorkspaceSnapshot,
        prefix: &str,
        outcome: CommandOutcome,
    ) -> Result<()> {
        let failure_signature = outcome.failure_signature(prefix);
        let message = format_command_failure(prefix, &outcome);
        self.record_failure(snapshot, &failure_signature, message.clone());
        Err(anyhow::anyhow!(message))
    }

    fn record_failure(&mut self, snapshot: &WorkspaceSnapshot, signature: &str, message: String) {
        let attempts = self
            .failure
            .as_ref()
            .filter(|record| record.workspace_key == snapshot.workspace_key())
            .map(|record| record.attempts + 1)
            .unwrap_or(1);
        let paused = attempts >= self.config.autonomous.max_failures_per_state;
        let state_fingerprint = snapshot.state_fingerprint(Some(signature));

        self.failure = Some(FailureRecord {
            workspace_key: snapshot.workspace_key(),
            state_fingerprint,
            attempts,
            last_failure_signature: signature.to_string(),
            last_error: message.clone(),
            paused,
        });

        if paused {
            warn!(attempts, error = %message, "Autonomous state paused after repeated failures");
        } else {
            warn!(attempts, error = %message, "Autonomous iteration failed");
        }
    }

    async fn run_command(&self, spec: CommandSpec) -> Result<CommandOutcome> {
        self.executor.run(spec).await
    }

    fn require_success(&self, result: Result<CommandOutcome>) -> Result<CommandOutcome> {
        let outcome = result?;
        if outcome.success {
            Ok(outcome)
        } else {
            Err(anyhow::anyhow!(format_command_failure(
                "Command failed",
                &outcome
            )))
        }
    }
}

fn build_autonomous_query(
    snapshot: &WorkspaceSnapshot,
    previous_failure: Option<&FailureRecord>,
) -> String {
    let implemented = if snapshot.implemented_items.is_empty() {
        "- none recorded".to_string()
    } else {
        snapshot
            .implemented_items
            .iter()
            .map(|item| format!("- {}", item))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let pending = snapshot
        .pending_items
        .iter()
        .map(|item| format!("- {}", item))
        .collect::<Vec<_>>()
        .join("\n");
    let previous_failure_text = previous_failure
        .map(|record| {
            format!(
                "Previous failed attempt:\n- signature: {}\n- details: {}\n",
                record.last_failure_signature, record.last_error
            )
        })
        .unwrap_or_default();

    format!(
        "You are running in hermes-rs autonomous coding mode.\n\
Inspect the current workspace before making changes. Follow TODO.md as the task source of truth.\n\
Implement only the next pending item that is realistically achievable in one iteration.\n\
Use existing file, patch, and terminal tools as needed, but do not run git add, git commit, or git push.\n\
After completing the work, update TODO.md by moving completed work from Pending to Implemented.\n\
If the task cannot be completed safely, explain why in TODO.md and leave the item pending.\n\n\
Workspace root: {}\n\
Current branch: {}\n\
Current HEAD: {}\n\
Current git status:\n{}\n\
Current TODO.md path: {}\n\n\
Implemented items:\n{}\n\n\
Pending items:\n{}\n\n\
Full TODO.md contents:\n{}\n\n{}",
        snapshot.repo_root.display(),
        snapshot.branch,
        snapshot.head,
        empty_to_placeholder(&snapshot.worktree_status),
        snapshot.todo_path.display(),
        implemented,
        pending,
        snapshot.todo_contents,
        previous_failure_text,
    )
}

fn parse_todo_ledger(raw: &str) -> TodoLedger {
    enum Section {
        None,
        Implemented,
        Pending,
    }

    let mut section = Section::None;
    let mut implemented = Vec::new();
    let mut pending = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.eq_ignore_ascii_case("## Implemented") {
            section = Section::Implemented;
            continue;
        }
        if trimmed.eq_ignore_ascii_case("## Pending") {
            section = Section::Pending;
            continue;
        }
        if trimmed.starts_with("## ") {
            section = Section::None;
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }

        let item = trimmed
            .trim_start_matches("- [x]")
            .trim_start_matches("- [X]")
            .trim_start_matches("- [ ]")
            .trim_start_matches("-")
            .trim();

        if item.is_empty() {
            continue;
        }

        match section {
            Section::Implemented => implemented.push(item.to_string()),
            Section::Pending => pending.push(item.to_string()),
            Section::None => {}
        }
    }

    TodoLedger {
        implemented,
        pending,
    }
}

fn empty_to_placeholder(value: &str) -> &str {
    if value.trim().is_empty() {
        "(clean worktree)"
    } else {
        value
    }
}

fn hash_parts<'a>(parts: impl IntoIterator<Item = &'a str>) -> u64 {
    let mut hasher = DefaultHasher::new();
    for part in parts {
        part.hash(&mut hasher);
    }
    hasher.finish()
}

fn looks_like_nothing_to_commit(outcome: &CommandOutcome) -> bool {
    let output = format!("{}\n{}", outcome.stdout, outcome.stderr).to_ascii_lowercase();
    output.contains("nothing to commit") || output.contains("nothing added to commit")
}

fn format_command_failure(prefix: &str, outcome: &CommandOutcome) -> String {
    let mut message = String::from(prefix);
    if outcome.timed_out {
        message.push_str(": command timed out");
    } else if let Some(code) = outcome.exit_code {
        message.push_str(&format!(": exit code {}", code));
    }

    let stdout = outcome.stdout.trim();
    if !stdout.is_empty() {
        message.push_str(&format!("\nstdout:\n{}", stdout));
    }
    let stderr = outcome.stderr.trim();
    if !stderr.is_empty() {
        message.push_str(&format!("\nstderr:\n{}", stderr));
    }
    message
}

fn exec_git(description: &str, cwd: &Path, args: &[&str], timeout: Duration) -> CommandSpec {
    exec_git_owned(
        description,
        cwd,
        args.iter().map(|arg| (*arg).to_string()).collect(),
        timeout,
    )
}

fn exec_git_owned(
    description: &str,
    cwd: &Path,
    args: Vec<String>,
    timeout: Duration,
) -> CommandSpec {
    CommandSpec {
        description: description.to_string(),
        cwd: cwd.to_path_buf(),
        timeout,
        kind: CommandKind::Exec {
            program: "git".to_string(),
            args,
        },
    }
}

async fn run_blocking_command(spec: CommandSpec) -> Result<CommandOutcome> {
    tokio::task::spawn_blocking(move || {
        let mut command = match spec.kind {
            CommandKind::Exec { program, args } => {
                let mut command = std::process::Command::new(program);
                command.args(args);
                command
            }
            CommandKind::Shell { command } => {
                let shell = detect_shell();
                let mut process = std::process::Command::new(shell.path);
                process.args(shell.args_pattern).arg(command);
                process
            }
        };

        command
            .current_dir(&spec.cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command
            .spawn()
            .with_context(|| format!("Failed to spawn '{}'", spec.description))?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let stdout_reader = thread::spawn(move || read_output(stdout));
        let stderr_reader = thread::spawn(move || read_output(stderr));
        let deadline = Instant::now() + spec.timeout;

        let status = loop {
            if let Some(status) = child
                .try_wait()
                .with_context(|| format!("Failed while polling '{}'", spec.description))?
            {
                break (status, false);
            }

            if Instant::now() >= deadline {
                let _ = child.kill();
                let status = child
                    .wait()
                    .with_context(|| format!("Failed to reap timed out '{}'", spec.description))?;
                break (status, true);
            }

            thread::sleep(Duration::from_millis(100));
        };

        let stdout = stdout_reader.join().unwrap_or_default();
        let stderr = stderr_reader.join().unwrap_or_default();

        Ok(CommandOutcome {
            success: status.0.success() && !status.1,
            exit_code: status.0.code(),
            stdout,
            stderr,
            timed_out: status.1,
        })
    })
    .await
    .context("Blocking command task failed")?
}

fn read_output(stream: Option<impl Read>) -> String {
    let Some(mut stream) = stream else {
        return String::new();
    };

    let mut buffer = Vec::new();
    if stream.read_to_end(&mut buffer).is_ok() {
        String::from_utf8_lossy(&buffer).to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default)]
    struct FakeCommandExecutor {
        calls: Arc<Mutex<Vec<String>>>,
        outcomes: Arc<Mutex<VecDeque<Result<CommandOutcome>>>>,
    }

    impl FakeCommandExecutor {
        fn with_outcomes(outcomes: Vec<Result<CommandOutcome>>) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                outcomes: Arc::new(Mutex::new(outcomes.into())),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl CommandExecutor for FakeCommandExecutor {
        async fn run(&self, spec: CommandSpec) -> Result<CommandOutcome> {
            self.calls.lock().unwrap().push(spec.description);
            self.outcomes
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(CommandOutcome {
                        success: true,
                        exit_code: Some(0),
                        stdout: String::new(),
                        stderr: String::new(),
                        timed_out: false,
                    })
                })
        }
    }

    fn success(stdout: &str) -> Result<CommandOutcome> {
        Ok(CommandOutcome {
            success: true,
            exit_code: Some(0),
            stdout: stdout.to_string(),
            stderr: String::new(),
            timed_out: false,
        })
    }

    fn failure(stderr: &str) -> Result<CommandOutcome> {
        Ok(CommandOutcome {
            success: false,
            exit_code: Some(1),
            stdout: String::new(),
            stderr: stderr.to_string(),
            timed_out: false,
        })
    }

    fn create_temp_repo() -> PathBuf {
        let repo_root = std::env::temp_dir().join(format!(
            "hermes_autonomous_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&repo_root).unwrap();
        std::fs::write(
            repo_root.join("TODO.md"),
            "## Implemented\n- done\n\n## Pending\n- build autonomous mode\n",
        )
        .unwrap();
        repo_root
    }

    fn snapshot_with_pending() -> WorkspaceSnapshot {
        let repo_root = create_temp_repo();
        WorkspaceSnapshot {
            repo_root: repo_root.clone(),
            todo_path: repo_root.join("TODO.md"),
            todo_contents: "## Implemented\n- done\n\n## Pending\n- build autonomous mode\n"
                .to_string(),
            branch: "main".to_string(),
            head: "abc123".to_string(),
            worktree_status: " M src/main.rs\n".to_string(),
            implemented_items: vec!["done".to_string()],
            pending_items: vec!["build autonomous mode".to_string()],
        }
    }

    #[test]
    fn todo_ledger_parses_sections() {
        let ledger = parse_todo_ledger(
            "# TODO\n\n## Implemented\n- [x] existing orchestration\n\n## Pending\n- [ ] add autonomous mode\n- update docs\n",
        );

        assert_eq!(ledger.implemented, vec!["existing orchestration"]);
        assert_eq!(ledger.pending, vec!["add autonomous mode", "update docs"]);
    }

    #[test]
    fn state_fingerprint_changes_with_failure_signature() {
        let snapshot = snapshot_with_pending();
        let base = snapshot.state_fingerprint(None);
        let failed = snapshot.state_fingerprint(Some("tests failed"));
        assert_ne!(base, failed);
    }

    #[tokio::test]
    async fn publish_changes_runs_git_steps_in_order() {
        let executor = FakeCommandExecutor::with_outcomes(vec![
            success(""),
            success("[main abc123] commit"),
            success("main\n"),
            success("def456\n"),
            success(""),
            success("pushed"),
        ]);
        let snapshot = snapshot_with_pending();
        let mut runner = AutonomousRunner::new(
            AppConfig::default(),
            None,
            snapshot.repo_root.clone(),
            executor,
        );
        runner.publish_changes(&snapshot).await.unwrap();

        assert_eq!(
            runner.executor.calls(),
            vec![
                "git add",
                "git commit",
                "read git branch",
                "read git head",
                "read git status",
                "git push",
            ]
        );
    }

    #[tokio::test]
    async fn failed_commit_blocks_push() {
        let executor =
            FakeCommandExecutor::with_outcomes(vec![success(""), failure("commit failed")]);
        let snapshot = snapshot_with_pending();
        let mut runner = AutonomousRunner::new(
            AppConfig::default(),
            None,
            snapshot.repo_root.clone(),
            executor,
        );

        assert!(runner.publish_changes(&snapshot).await.is_err());
        assert_eq!(runner.executor.calls(), vec!["git add", "git commit"]);
    }

    #[test]
    fn failure_record_pauses_at_threshold() {
        let mut runner = AutonomousRunner::new(
            AppConfig::default(),
            None,
            PathBuf::from("."),
            FakeCommandExecutor::default(),
        );
        let snapshot = snapshot_with_pending();

        runner.record_failure(&snapshot, "tests failed", "tests failed".to_string());
        assert_eq!(runner.failure.as_ref().unwrap().attempts, 1);
        assert!(!runner.failure.as_ref().unwrap().paused);

        runner.record_failure(&snapshot, "tests failed", "tests failed".to_string());
        runner.record_failure(&snapshot, "tests failed", "tests failed".to_string());
        assert!(runner.failure.as_ref().unwrap().paused);
    }
}
