use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use async_trait::async_trait;
use hermes_core::config::AppConfig;
use hermes_core::platform::detect_shell;
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AutonomousState {
    #[default]
    Idle,
    Running,
    Failed,
    Paused,
    Succeeded,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct StatusCommandSummary {
    success: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

impl From<&CommandOutcome> for StatusCommandSummary {
    fn from(outcome: &CommandOutcome) -> Self {
        Self {
            success: outcome.success,
            exit_code: outcome.exit_code,
            stdout: outcome.stdout.clone(),
            stderr: outcome.stderr.clone(),
            timed_out: outcome.timed_out,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct StatusPublishSummary {
    remote: String,
    branch: String,
    commit_message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct AutonomousStatusReport {
    state: AutonomousState,
    repo_root: Option<PathBuf>,
    todo_path: Option<PathBuf>,
    branch: Option<String>,
    head: Option<String>,
    implemented_items: Vec<String>,
    pending_items: Vec<String>,
    last_tick_started_unix_secs: Option<u64>,
    last_tick_completed_unix_secs: Option<u64>,
    last_success_unix_secs: Option<u64>,
    attempts: usize,
    last_failure_signature: Option<String>,
    last_error: Option<String>,
    paused: bool,
    workspace_key: Option<String>,
    state_fingerprint: Option<String>,
    last_validation: Option<StatusCommandSummary>,
    last_publish: Option<StatusPublishSummary>,
}

impl AutonomousStatusReport {
    fn failure_record(&self) -> Option<FailureRecord> {
        Some(FailureRecord {
            workspace_key: self.workspace_key.as_deref()?.parse().ok()?,
            state_fingerprint: self.state_fingerprint.as_deref()?.parse().ok()?,
            attempts: self.attempts,
            last_failure_signature: self.last_failure_signature.clone()?,
            last_error: self.last_error.clone().unwrap_or_default(),
            paused: self.paused,
        })
        .filter(|record| record.attempts > 0)
    }
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

#[async_trait(?Send)]
trait AutonomousAgentExecutor: Send + Sync {
    async fn run(&self, snapshot: &WorkspaceSnapshot, query: String) -> Result<()>;
}

#[derive(Debug, Clone, Copy, Default)]
struct RealCommandExecutor;

#[async_trait]
impl CommandExecutor for RealCommandExecutor {
    async fn run(&self, spec: CommandSpec) -> Result<CommandOutcome> {
        run_blocking_command(spec).await
    }
}

#[derive(Debug, Clone)]
struct RealAutonomousAgentExecutor {
    config: AppConfig,
    system_prompt: Option<String>,
}

#[async_trait(?Send)]
impl AutonomousAgentExecutor for RealAutonomousAgentExecutor {
    async fn run(&self, _snapshot: &WorkspaceSnapshot, query: String) -> Result<()> {
        let mut mcp_manager = McpManager::new();
        let agent = create_agent_without_events(
            &self.config,
            self.system_prompt.as_deref(),
            &mut mcp_manager,
        )
        .await?;
        agent.run(query).await?;
        Ok(())
    }
}

pub async fn run_autonomous(config: AppConfig, system_prompt: Option<String>) -> Result<()> {
    let repo_root = std::env::current_dir().context("Failed to determine current directory")?;
    let agent_executor = RealAutonomousAgentExecutor {
        config: config.clone(),
        system_prompt,
    };
    let mut runner = AutonomousRunner::new(config, repo_root, RealCommandExecutor, agent_executor);
    runner.run_loop().await
}

struct AutonomousRunner<E, A> {
    config: AppConfig,
    repo_root: PathBuf,
    executor: E,
    agent_executor: A,
    failure: Option<FailureRecord>,
    status_path: PathBuf,
    last_success_unix_secs: Option<u64>,
}

enum PublishOutcome {
    Failed,
    Skipped(WorkspaceSnapshot),
    Pushed(WorkspaceSnapshot, StatusPublishSummary),
}

impl<E, A> AutonomousRunner<E, A>
where
    E: CommandExecutor,
    A: AutonomousAgentExecutor,
{
    fn new(config: AppConfig, repo_root: PathBuf, executor: E, agent_executor: A) -> Self {
        let status_path = resolve_workspace_path(&repo_root, &config.autonomous.status_path);
        let (failure, last_success_unix_secs) = match load_status_report(&status_path) {
            Ok(Some(status)) => (status.failure_record(), status.last_success_unix_secs),
            Ok(None) => (None, None),
            Err(load_error) => {
                warn!(
                    path = %status_path.display(),
                    error = %load_error,
                    "Failed to load autonomous status file; starting with a fresh runtime state",
                );
                (None, None)
            }
        };

        Self {
            config,
            repo_root,
            executor,
            agent_executor,
            failure,
            status_path,
            last_success_unix_secs,
        }
    }

    async fn run_loop(&mut self) -> Result<()> {
        info!(
            interval_secs = self.config.autonomous.interval_secs,
            todo = %self.todo_path().display(),
            status = %self.status_path.display(),
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
        let tick_started_at = unix_timestamp_secs();
        let snapshot = match self.capture_workspace_snapshot(tick_started_at).await? {
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
                status = %self.status_path.display(),
                "Autonomous state is paused until TODO.md or git state changes",
            );
            self.write_status_report(self.build_status_report(
                AutonomousState::Paused,
                Some(&snapshot),
                tick_started_at,
                None,
                None,
            ))
            .await?;
            return Ok(());
        }

        if prior_failure.is_some_and(|record| record.paused) {
            info!(
                status = %self.status_path.display(),
                "Autonomous pause cleared after workspace state changed",
            );
            self.failure = None;
        }

        let prior_failure = self
            .failure
            .as_ref()
            .filter(|record| record.workspace_key == snapshot.workspace_key());
        let query = build_autonomous_query(&snapshot, prior_failure);
        let previous_status = snapshot.worktree_status.clone();

        self.write_status_report(self.build_status_report(
            AutonomousState::Running,
            Some(&snapshot),
            tick_started_at,
            None,
            None,
        ))
        .await?;

        match self.agent_executor.run(&snapshot, query).await {
            Ok(_) => {}
            Err(error) => {
                let error_text = format!("Agent run failed: {}", error);
                self.record_failure(
                    &snapshot,
                    "agent run failed",
                    error_text,
                    tick_started_at,
                    None,
                    None,
                )
                .await?;
                return Ok(());
            }
        }

        let current_status = self.git_status().await?;
        if current_status == previous_status {
            self.record_failure(
                &snapshot,
                "no workspace changes",
                "Agent produced no workspace changes.".to_string(),
                tick_started_at,
                None,
                None,
            )
            .await?;
            return Ok(());
        }

        let post_snapshot = self
            .refresh_snapshot_after_run(&snapshot, current_status)
            .await?;

        let validation = self.run_validation().await?;
        let validation_summary = StatusCommandSummary::from(&validation);
        if !validation.success {
            let failure_signature = validation.failure_signature("validation");
            let message = format_command_failure("Validation failed", &validation);
            self.record_failure(
                &post_snapshot,
                &failure_signature,
                message,
                tick_started_at,
                Some(validation_summary),
                None,
            )
            .await?;
            return Ok(());
        }

        match self
            .publish_changes(
                &post_snapshot,
                tick_started_at,
                Some(validation_summary.clone()),
            )
            .await?
        {
            PublishOutcome::Failed => return Ok(()),
            PublishOutcome::Skipped(final_snapshot) => {
                self.finish_success(
                    &final_snapshot,
                    tick_started_at,
                    Some(validation_summary),
                    None,
                )
                .await?;
                info!("Autonomous iteration completed without new publish work");
            }
            PublishOutcome::Pushed(final_snapshot, publish_summary) => {
                self.finish_success(
                    &final_snapshot,
                    tick_started_at,
                    Some(validation_summary),
                    Some(publish_summary),
                )
                .await?;
                info!("Autonomous iteration completed and pushed successfully");
            }
        }
        Ok(())
    }

    async fn capture_workspace_snapshot(
        &mut self,
        tick_started_at: u64,
    ) -> Result<Option<WorkspaceSnapshot>> {
        let todo_path = self.todo_path();
        let snapshot = match self.refresh_snapshot(todo_path.clone()).await {
            Ok(snapshot) => snapshot,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|io_error| io_error.kind() == std::io::ErrorKind::NotFound) =>
            {
                warn!(path = %todo_path.display(), "TODO.md not found; skipping autonomous tick");
                self.write_status_report(self.build_status_report(
                    AutonomousState::Idle,
                    None,
                    tick_started_at,
                    None,
                    None,
                ))
                .await?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        };

        if snapshot.pending_items.is_empty() {
            info!(path = %todo_path.display(), "TODO.md has no pending items; skipping autonomous tick");
            self.failure = None;
            self.write_status_report(self.build_status_report(
                AutonomousState::Idle,
                Some(&snapshot),
                tick_started_at,
                None,
                None,
            ))
            .await?;
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
        resolve_workspace_path(&self.repo_root, &self.config.autonomous.todo_path)
    }

    fn command_timeout(&self) -> Duration {
        Duration::from_secs(self.config.autonomous.command_timeout_secs)
    }

    fn publish_summary(&self) -> StatusPublishSummary {
        StatusPublishSummary {
            remote: self.config.autonomous.git_remote.clone(),
            branch: self.config.autonomous.git_branch.clone(),
            commit_message: self.config.autonomous.commit_message.clone(),
        }
    }

    fn relative_status_path(&self) -> Option<String> {
        self.status_path
            .strip_prefix(&self.repo_root)
            .ok()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
    }

    fn filter_status_output(&self, raw: &str) -> String {
        let Some(relative_status_path) = self.relative_status_path() else {
            return raw.to_string();
        };

        let filtered = raw
            .lines()
            .filter(|line| !line.replace('\\', "/").contains(&relative_status_path))
            .collect::<Vec<_>>();

        if filtered.is_empty() {
            String::new()
        } else {
            format!("{}\n", filtered.join("\n"))
        }
    }

    async fn git_status(&self) -> Result<String> {
        let raw = self
            .require_success(
                self.run_command(exec_git(
                    "read git status",
                    &self.repo_root,
                    &["status", "--short"],
                    self.command_timeout(),
                ))
                .await,
            )?
            .stdout;
        Ok(self.filter_status_output(&raw))
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

    async fn publish_changes(
        &mut self,
        snapshot: &WorkspaceSnapshot,
        tick_started_at: u64,
        validation: Option<StatusCommandSummary>,
    ) -> Result<PublishOutcome> {
        let mut add_args = vec!["add".to_string(), "--all".to_string(), ".".to_string()];
        if let Some(relative_status_path) = self.relative_status_path() {
            add_args.push(format!(":(exclude){}", relative_status_path));
        }
        let add = self
            .run_command(exec_git_owned(
                "git add",
                &self.repo_root,
                add_args,
                self.command_timeout(),
            ))
            .await?;
        if !add.success {
            self.fail_publish(
                snapshot,
                "git add failed",
                add,
                tick_started_at,
                validation,
                None,
            )
            .await?;
            return Ok(PublishOutcome::Failed);
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
                let refreshed_snapshot = self.refresh_snapshot(snapshot.todo_path.clone()).await?;
                return Ok(PublishOutcome::Skipped(refreshed_snapshot));
            }
            self.fail_publish(
                snapshot,
                "git commit failed",
                commit,
                tick_started_at,
                validation,
                None,
            )
            .await?;
            return Ok(PublishOutcome::Failed);
        }

        let committed_snapshot = self.refresh_snapshot(snapshot.todo_path.clone()).await?;
        let publish_summary = self.publish_summary();

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
            self.fail_publish(
                &committed_snapshot,
                "git push failed",
                push,
                tick_started_at,
                validation,
                Some(publish_summary),
            )
            .await?;
            return Ok(PublishOutcome::Failed);
        }

        let pushed_snapshot = self.refresh_snapshot(snapshot.todo_path.clone()).await?;
        Ok(PublishOutcome::Pushed(pushed_snapshot, publish_summary))
    }

    async fn finish_success(
        &mut self,
        snapshot: &WorkspaceSnapshot,
        tick_started_at: u64,
        validation: Option<StatusCommandSummary>,
        publish: Option<StatusPublishSummary>,
    ) -> Result<()> {
        self.failure = None;
        self.last_success_unix_secs = Some(unix_timestamp_secs());
        self.write_status_report(self.build_status_report(
            AutonomousState::Succeeded,
            Some(snapshot),
            tick_started_at,
            validation,
            publish,
        ))
        .await
    }

    async fn fail_publish(
        &mut self,
        snapshot: &WorkspaceSnapshot,
        prefix: &str,
        outcome: CommandOutcome,
        tick_started_at: u64,
        validation: Option<StatusCommandSummary>,
        publish: Option<StatusPublishSummary>,
    ) -> Result<()> {
        let failure_signature = outcome.failure_signature(prefix);
        let message = format_command_failure(prefix, &outcome);
        self.record_failure(
            snapshot,
            &failure_signature,
            message,
            tick_started_at,
            validation,
            publish,
        )
        .await
    }

    async fn record_failure(
        &mut self,
        snapshot: &WorkspaceSnapshot,
        signature: &str,
        message: String,
        tick_started_at: u64,
        validation: Option<StatusCommandSummary>,
        publish: Option<StatusPublishSummary>,
    ) -> Result<()> {
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
            warn!(
                attempts,
                error = %message,
                status = %self.status_path.display(),
                "Autonomous state paused after repeated failures",
            );
        } else {
            warn!(
                attempts,
                error = %message,
                status = %self.status_path.display(),
                "Autonomous iteration failed",
            );
        }

        self.write_status_report(self.build_status_report(
            if paused {
                AutonomousState::Paused
            } else {
                AutonomousState::Failed
            },
            Some(snapshot),
            tick_started_at,
            validation,
            publish,
        ))
        .await
    }

    fn build_status_report(
        &self,
        state: AutonomousState,
        snapshot: Option<&WorkspaceSnapshot>,
        tick_started_at: u64,
        validation: Option<StatusCommandSummary>,
        publish: Option<StatusPublishSummary>,
    ) -> AutonomousStatusReport {
        let mut report = AutonomousStatusReport {
            state: state.clone(),
            repo_root: Some(self.repo_root.clone()),
            todo_path: Some(self.todo_path()),
            last_tick_started_unix_secs: Some(tick_started_at),
            last_tick_completed_unix_secs: (!matches!(state, AutonomousState::Running))
                .then_some(unix_timestamp_secs()),
            last_success_unix_secs: self.last_success_unix_secs,
            last_validation: validation,
            last_publish: publish,
            ..AutonomousStatusReport::default()
        };

        if let Some(snapshot) = snapshot {
            report.repo_root = Some(snapshot.repo_root.clone());
            report.todo_path = Some(snapshot.todo_path.clone());
            report.branch = Some(snapshot.branch.clone());
            report.head = Some(snapshot.head.clone());
            report.implemented_items = snapshot.implemented_items.clone();
            report.pending_items = snapshot.pending_items.clone();
            report.workspace_key = Some(snapshot.workspace_key().to_string());
        }

        if matches!(state, AutonomousState::Failed | AutonomousState::Paused) {
            if let Some(failure) = &self.failure {
                report.attempts = failure.attempts;
                report.last_failure_signature = Some(failure.last_failure_signature.clone());
                report.last_error = Some(failure.last_error.clone());
                report.paused = failure.paused;
                report.workspace_key = Some(failure.workspace_key.to_string());
                report.state_fingerprint = Some(failure.state_fingerprint.to_string());
            }
        }

        report
    }

    async fn write_status_report(&self, report: AutonomousStatusReport) -> Result<()> {
        let mut raw =
            toml::to_string_pretty(&report).context("Failed to serialize status report")?;
        if !raw.ends_with('\n') {
            raw.push('\n');
        }

        if let Some(parent) = self.status_path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!(
                    "Failed to create autonomous status directory '{}'",
                    parent.display()
                )
            })?;
        }

        let temp_path = temp_output_path(&self.status_path);
        fs::write(&temp_path, raw).await.with_context(|| {
            format!(
                "Failed to write autonomous status temp file '{}'",
                temp_path.display()
            )
        })?;

        if let Err(rename_error) = fs::rename(&temp_path, &self.status_path).await {
            if self.status_path.exists() {
                let _ = fs::remove_file(&self.status_path).await;
                fs::rename(&temp_path, &self.status_path)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to replace autonomous status file '{}' after rename error: {}",
                            self.status_path.display(),
                            rename_error
                        )
                    })?;
            } else {
                return Err(rename_error).with_context(|| {
                    format!(
                        "Failed to move autonomous status file into place '{}'",
                        self.status_path.display()
                    )
                });
            }
        }

        Ok(())
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

fn resolve_workspace_path(repo_root: &Path, configured: &Path) -> PathBuf {
    if configured.is_absolute() {
        configured.to_path_buf()
    } else {
        repo_root.join(configured)
    }
}

fn load_status_report(path: &Path) -> Result<Option<AutonomousStatusReport>> {
    if !path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read autonomous status '{}'", path.display()))?;
    let status = toml::from_str::<AutonomousStatusReport>(&raw)
        .with_context(|| format!("Failed to parse autonomous status '{}'", path.display()))?;
    Ok(Some(status))
}

fn temp_output_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("autonomous-status.toml");
    path.with_file_name(format!(
        ".{}.{}.tmp",
        file_name,
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ))
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};
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

    #[derive(Debug, Clone, Copy, Default)]
    struct PushlessRealCommandExecutor;

    #[async_trait]
    impl CommandExecutor for PushlessRealCommandExecutor {
        async fn run(&self, spec: CommandSpec) -> Result<CommandOutcome> {
            if spec.description == "git push" {
                return Ok(CommandOutcome {
                    success: true,
                    exit_code: Some(0),
                    stdout: "pushed".to_string(),
                    stderr: String::new(),
                    timed_out: false,
                });
            }

            run_blocking_command(spec).await
        }
    }

    #[derive(Debug, Clone, Copy, Default)]
    struct NoopAgentExecutor;

    #[async_trait(?Send)]
    impl AutonomousAgentExecutor for NoopAgentExecutor {
        async fn run(&self, _snapshot: &WorkspaceSnapshot, _query: String) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Debug, Clone)]
    struct EditingAgentExecutor {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait(?Send)]
    impl AutonomousAgentExecutor for EditingAgentExecutor {
        async fn run(&self, snapshot: &WorkspaceSnapshot, query: String) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert!(query.contains("Pending items"));
            std::fs::write(
                snapshot.repo_root.join("notes.txt"),
                "autonomous change applied\n",
            )?;
            std::fs::write(
                &snapshot.todo_path,
                "## Implemented\n- setup repo\n- automate sample task\n\n## Pending\n",
            )?;
            Ok(())
        }
    }

    #[derive(Debug, Clone)]
    struct FailingAgentExecutor {
        calls: Arc<AtomicUsize>,
        message: String,
    }

    #[async_trait(?Send)]
    impl AutonomousAgentExecutor for FailingAgentExecutor {
        async fn run(&self, _snapshot: &WorkspaceSnapshot, _query: String) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(anyhow::anyhow!(self.message.clone()))
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

    fn unique_temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "hermes_autonomous_{}_{}_{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn create_temp_repo() -> PathBuf {
        let repo_root = unique_temp_path("unit");
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

    fn sample_config() -> AppConfig {
        let mut config = AppConfig::default();
        config.autonomous.todo_path = PathBuf::from("TODO.md");
        config.autonomous.status_path = PathBuf::from("autonomous-status.toml");
        config.autonomous.test_command = "git diff --check".to_string();
        config.autonomous.git_remote = "origin".to_string();
        config.autonomous.git_branch = "agent-dev".to_string();
        config.autonomous.commit_message = "Auto-commit by hermes-rs".to_string();
        config.autonomous.command_timeout_secs = 60;
        config.autonomous.max_failures_per_state = 3;
        config
    }

    fn read_status(repo_root: &Path) -> AutonomousStatusReport {
        load_status_report(&repo_root.join("autonomous-status.toml"))
            .unwrap()
            .unwrap()
    }

    fn git_output(repo_root: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args(args)
            .current_dir(repo_root)
            .output()
            .unwrap_or_else(|error| panic!("failed to run git {:?}: {}", args, error))
    }

    fn git_success(repo_root: &Path, args: &[&str]) {
        let output = git_output(repo_root, args);
        assert!(
            output.status.success(),
            "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_disposable_repo(todo_contents: &str) -> PathBuf {
        let repo_root = unique_temp_path("repo");
        Command::new("git")
            .arg("init")
            .arg(&repo_root)
            .output()
            .unwrap();

        git_success(&repo_root, &["config", "user.name", "Hermes Test"]);
        git_success(&repo_root, &["config", "user.email", "hermes@example.com"]);
        git_success(&repo_root, &["checkout", "-b", "agent-dev"]);

        std::fs::write(repo_root.join("TODO.md"), todo_contents).unwrap();
        std::fs::write(repo_root.join("README.md"), "sample repo\n").unwrap();

        git_success(&repo_root, &["add", "."]);
        git_success(&repo_root, &["commit", "-m", "Initial commit"]);

        repo_root
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
            success("main\n"),
            success("ghi789\n"),
            success(""),
        ]);
        let snapshot = snapshot_with_pending();
        let mut runner = AutonomousRunner::new(
            sample_config(),
            snapshot.repo_root.clone(),
            executor,
            NoopAgentExecutor,
        );
        let outcome = runner
            .publish_changes(
                &snapshot,
                unix_timestamp_secs(),
                Some(StatusCommandSummary::from(&CommandOutcome {
                    success: true,
                    exit_code: Some(0),
                    stdout: String::new(),
                    stderr: String::new(),
                    timed_out: false,
                })),
            )
            .await
            .unwrap();

        assert!(matches!(outcome, PublishOutcome::Pushed(_, _)));

        assert_eq!(
            runner.executor.calls(),
            vec![
                "git add",
                "git commit",
                "read git branch",
                "read git head",
                "read git status",
                "git push",
                "read git branch",
                "read git head",
                "read git status",
            ]
        );
    }

    #[tokio::test]
    async fn failed_commit_blocks_push() {
        let executor =
            FakeCommandExecutor::with_outcomes(vec![success(""), failure("commit failed")]);
        let snapshot = snapshot_with_pending();
        let mut runner = AutonomousRunner::new(
            sample_config(),
            snapshot.repo_root.clone(),
            executor,
            NoopAgentExecutor,
        );

        let outcome = runner
            .publish_changes(
                &snapshot,
                unix_timestamp_secs(),
                Some(StatusCommandSummary::default()),
            )
            .await
            .unwrap();
        assert!(matches!(outcome, PublishOutcome::Failed));
        assert_eq!(runner.executor.calls(), vec!["git add", "git commit"]);
    }

    #[tokio::test]
    async fn failure_record_pauses_at_threshold() {
        let repo_root = create_temp_repo();
        let mut runner = AutonomousRunner::new(
            sample_config(),
            repo_root,
            FakeCommandExecutor::default(),
            NoopAgentExecutor,
        );
        let snapshot = snapshot_with_pending();

        runner
            .record_failure(
                &snapshot,
                "tests failed",
                "tests failed".to_string(),
                unix_timestamp_secs(),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(runner.failure.as_ref().unwrap().attempts, 1);
        assert!(!runner.failure.as_ref().unwrap().paused);

        runner
            .record_failure(
                &snapshot,
                "tests failed",
                "tests failed".to_string(),
                unix_timestamp_secs(),
                None,
                None,
            )
            .await
            .unwrap();
        runner
            .record_failure(
                &snapshot,
                "tests failed",
                "tests failed".to_string(),
                unix_timestamp_secs(),
                None,
                None,
            )
            .await
            .unwrap();
        assert!(runner.failure.as_ref().unwrap().paused);
    }

    #[tokio::test]
    async fn autonomous_tick_updates_disposable_repo_and_status_report() {
        let repo_root = init_disposable_repo(
            "## Implemented\n- setup repo\n\n## Pending\n- automate sample task\n",
        );
        let call_count = Arc::new(AtomicUsize::new(0));
        let agent = EditingAgentExecutor {
            calls: call_count.clone(),
        };
        let mut runner = AutonomousRunner::new(
            sample_config(),
            repo_root.clone(),
            PushlessRealCommandExecutor,
            agent,
        );

        runner.tick().await.unwrap();

        assert_eq!(call_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            std::fs::read_to_string(repo_root.join("notes.txt")).unwrap(),
            "autonomous change applied\n"
        );
        assert!(std::fs::read_to_string(repo_root.join("TODO.md"))
            .unwrap()
            .contains("- automate sample task"));

        let status = read_status(&repo_root);
        assert_eq!(status.state, AutonomousState::Succeeded);
        assert!(status.pending_items.is_empty());
        assert_eq!(
            status
                .last_publish
                .as_ref()
                .map(|publish| publish.branch.as_str()),
            Some("agent-dev")
        );
        assert!(status
            .last_validation
            .as_ref()
            .is_some_and(|validation| validation.success));

    }

    #[tokio::test]
    async fn persisted_pause_state_survives_restart_until_workspace_changes() {
        let repo_root =
            init_disposable_repo("## Implemented\n- setup repo\n\n## Pending\n- blocked task\n");
        let call_count = Arc::new(AtomicUsize::new(0));
        let agent = FailingAgentExecutor {
            calls: call_count.clone(),
            message: "planned failure".to_string(),
        };

        let mut runner = AutonomousRunner::new(
            sample_config(),
            repo_root.clone(),
            RealCommandExecutor,
            agent.clone(),
        );
        runner.tick().await.unwrap();
        runner.tick().await.unwrap();
        runner.tick().await.unwrap();

        let paused_status = read_status(&repo_root);
        assert_eq!(paused_status.state, AutonomousState::Paused);
        assert_eq!(paused_status.attempts, 3);
        assert!(paused_status.paused);
        assert_eq!(call_count.load(Ordering::SeqCst), 3);

        let mut resumed_runner = AutonomousRunner::new(
            sample_config(),
            repo_root.clone(),
            RealCommandExecutor,
            agent.clone(),
        );
        resumed_runner.tick().await.unwrap();
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
        assert_eq!(read_status(&repo_root).state, AutonomousState::Paused);

        std::fs::write(
            repo_root.join("TODO.md"),
            "## Implemented\n- setup repo\n\n## Pending\n- blocked task\n- retry after edit\n",
        )
        .unwrap();

        resumed_runner.tick().await.unwrap();

        assert_eq!(call_count.load(Ordering::SeqCst), 4);
        let resumed_status = read_status(&repo_root);
        assert_eq!(resumed_status.state, AutonomousState::Failed);
        assert_eq!(resumed_status.attempts, 1);
        assert!(!resumed_status.paused);
    }
}
