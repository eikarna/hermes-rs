//! Cron/scheduler (F5).
//!
//! User-defined recurring prompts delivered into a chat channel on an
//! interval. Jobs persist to `~/.hermes-rs/cron/jobs.json` (atomic writes
//! via [`crate::persist`]) and survive restarts.
//!
//! Interval syntax is stdlib-only: a sequence of `<number><unit>` pairs —
//! `s` (seconds), `m` (minutes), `h` (hours), `d` (days). Examples:
//! `30m`, `2h`, `1d`, `1h30m`. Minimum interval is 60 seconds to keep a
//! typo from hot-looping the agent.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::persist;

/// Minimum allowed interval (guards against `1s` hot loops).
pub const MIN_INTERVAL_SECS: u64 = 60;

/// One recurring job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    /// Stable numeric id (shown in `/cron list`, used by `/cron remove`).
    pub id: u64,
    /// The prompt sent to the agent each fire.
    pub prompt: String,
    /// Interval between fires, in seconds.
    pub interval_secs: u64,
    /// Platform the job delivers to (e.g. "telegram").
    pub platform: String,
    /// Channel/chat id the job delivers to.
    pub channel_id: String,
    /// Unix timestamp of the next scheduled fire.
    pub next_run: i64,
    /// Paused jobs stay stored but never fire.
    pub enabled: bool,
}

/// Persisted job table.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct JobTable {
    /// Next id to hand out.
    next_id: u64,
    jobs: Vec<CronJob>,
}

/// File-backed scheduler state.
///
/// All mutations are persisted via atomic writes (temp file + rename), so a
/// crash mid-write can never corrupt the job table.
#[derive(Debug)]
pub struct Scheduler {
    state: tokio::sync::Mutex<JobTable>,
    dir: std::path::PathBuf,
}

impl Scheduler {
    pub fn new(dir: std::path::PathBuf) -> Self {
        let state = persist::read_json::<JobTable>(&dir.join("jobs.json")).unwrap_or_default();
        Self {
            state: tokio::sync::Mutex::new(state),
            dir,
        }
    }

    pub fn default_dir() -> std::path::PathBuf {
        persist::data_dir("cron")
    }

    fn path(&self) -> std::path::PathBuf {
        self.dir.join("jobs.json")
    }

    fn save(&self, table: &JobTable) -> Result<()> {
        persist::write_json(&self.path(), table)
            .map_err(|e| Error::Agent(format!("Failed to persist cron jobs: {}", e)))
    }

    /// Add a job firing every `interval_secs` into `platform:channel_id`.
    /// The first fire is one full interval from now.
    pub async fn add(
        &self,
        prompt: String,
        interval_secs: u64,
        platform: String,
        channel_id: String,
    ) -> Result<CronJob> {
        if interval_secs < MIN_INTERVAL_SECS {
            return Err(Error::Config(format!(
                "Interval must be at least {} seconds",
                MIN_INTERVAL_SECS
            )));
        }
        if prompt.trim().is_empty() {
            return Err(Error::Config("Job prompt must not be empty".to_string()));
        }
        let now = now_secs();
        let mut table = self.state.lock().await;
        let id = table.next_id;
        table.next_id += 1;
        let job = CronJob {
            id,
            prompt,
            interval_secs,
            platform,
            channel_id,
            next_run: now.saturating_add(interval_secs as i64),
            enabled: true,
        };
        table.jobs.push(job.clone());
        self.save(&table)?;
        Ok(job)
    }

    /// Remove a job by id. Returns the removed job, if any.
    pub async fn remove(&self, id: u64) -> Result<Option<CronJob>> {
        let mut table = self.state.lock().await;
        let removed = table
            .jobs
            .iter()
            .position(|j| j.id == id)
            .map(|pos| table.jobs.remove(pos));
        if removed.is_some() {
            self.save(&table)?;
        }
        Ok(removed)
    }

    /// Pause or resume a job by id.
    pub async fn set_enabled(&self, id: u64, enabled: bool) -> Result<bool> {
        let mut table = self.state.lock().await;
        let mut found = false;
        for job in &mut table.jobs {
            if job.id == id {
                job.enabled = enabled;
                if enabled {
                    // Resuming restarts the countdown from now.
                    job.next_run = now_secs().saturating_add(job.interval_secs as i64);
                }
                found = true;
            }
        }
        if found {
            self.save(&table)?;
        }
        Ok(found)
    }

    /// Snapshot of all jobs (for `/cron list`).
    pub async fn list(&self) -> Vec<CronJob> {
        self.state.lock().await.jobs.clone()
    }

    /// Jobs whose deadline has passed. Does not mutate state; call
    /// [`Scheduler::mark_fired`] after dispatching.
    pub async fn due_jobs(&self, now: i64) -> Vec<CronJob> {
        self.state
            .lock()
            .await
            .jobs
            .iter()
            .filter(|j| j.enabled && j.next_run <= now)
            .cloned()
            .collect()
    }

    /// Push a job's next fire one interval forward (repeatedly, so a long
    /// downtime burst fires once, not N times).
    pub async fn mark_fired(&self, id: u64) -> Result<()> {
        let mut table = self.state.lock().await;
        let now = now_secs();
        for job in &mut table.jobs {
            if job.id == id {
                let mut next = job.next_run + job.interval_secs as i64;
                while next <= now {
                    next += job.interval_secs as i64;
                }
                job.next_run = next;
            }
        }
        self.save(&table)
    }
}

/// Current unix time in seconds.
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Parse interval syntax: `30m`, `2h`, `1d`, `1h30m`, `90s`.
///
/// Returns seconds. Rejects empty input, unknown units, zero values, and
/// anything below [`MIN_INTERVAL_SECS`].
pub fn parse_interval(input: &str) -> Result<u64> {
    let input = input.trim();
    if input.is_empty() {
        return Err(Error::Config("Empty interval".to_string()));
    }

    let mut total: u64 = 0;
    let mut num: Option<u64> = None;
    let mut saw_any = false;

    for ch in input.chars() {
        if ch.is_ascii_digit() {
            let digit = ch.to_digit(10).unwrap() as u64;
            num = Some(
                num.unwrap_or(0)
                    .checked_mul(10)
                    .and_then(|n| n.checked_add(digit))
                    .ok_or_else(|| Error::Config("Interval number overflow".to_string()))?,
            );
            continue;
        }
        let unit_secs = match ch {
            's' => 1,
            'm' => 60,
            'h' => 3_600,
            'd' => 86_400,
            other => {
                return Err(Error::Config(format!(
                    "Unknown interval unit '{}'. Use s/m/h/d (e.g. 30m, 2h, 1d)",
                    other
                )));
            }
        };
        let value = num
            .take()
            .ok_or_else(|| Error::Config(format!("Unit '{}' needs a number before it", ch)))?;
        if value == 0 {
            return Err(Error::Config("Interval components must be > 0".to_string()));
        }
        total = total
            .checked_add(
                value
                    .checked_mul(unit_secs)
                    .ok_or_else(|| Error::Config("Interval overflow".to_string()))?,
            )
            .ok_or_else(|| Error::Config("Interval overflow".to_string()))?;
        saw_any = true;
    }

    if !saw_any || num.is_some() {
        return Err(Error::Config(
            "Interval must end in a unit (e.g. 30m, 2h, 1d)".to_string(),
        ));
    }
    if total < MIN_INTERVAL_SECS {
        return Err(Error::Config(format!(
            "Interval must be at least {} seconds",
            MIN_INTERVAL_SECS
        )));
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_units() {
        assert_eq!(parse_interval("90s").unwrap(), 90);
        assert_eq!(parse_interval("30m").unwrap(), 1_800);
        assert_eq!(parse_interval("2h").unwrap(), 7_200);
        assert_eq!(parse_interval("1d").unwrap(), 86_400);
    }

    #[test]
    fn parse_compound() {
        assert_eq!(parse_interval("1h30m").unwrap(), 5_400);
        assert_eq!(parse_interval("1d12h").unwrap(), 129_600);
    }

    #[test]
    fn parse_rejects_bad_input() {
        assert!(parse_interval("").is_err());
        assert!(parse_interval("30").is_err()); // no unit
        assert!(parse_interval("m").is_err()); // no number
        assert!(parse_interval("30x").is_err()); // bad unit
        assert!(parse_interval("0m").is_err()); // zero
        assert!(parse_interval("30s").is_err()); // below minimum
        assert!(parse_interval("59s").is_err()); // below minimum
    }

    fn temp_scheduler() -> (Scheduler, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let sched = Scheduler::new(dir.path().to_path_buf());
        (sched, dir)
    }

    #[tokio::test]
    async fn add_list_remove_roundtrip() {
        let (sched, _dir) = temp_scheduler();
        let job = sched
            .add(
                "check the news".into(),
                3600,
                "telegram".into(),
                "123".into(),
            )
            .await
            .unwrap();
        assert_eq!(job.id, 0);
        assert_eq!(sched.list().await.len(), 1);

        let removed = sched.remove(job.id).await.unwrap();
        assert!(removed.is_some());
        assert!(sched.list().await.is_empty());
        assert!(sched.remove(99).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn due_jobs_respects_enabled_and_deadline() {
        let (sched, _dir) = temp_scheduler();
        let job = sched
            .add("ping".into(), 60, "telegram".into(), "1".into())
            .await
            .unwrap();

        // Not due yet (first fire is one interval out).
        assert!(sched.due_jobs(now_secs()).await.is_empty());

        // Due when the clock passes next_run.
        let due = sched.due_jobs(job.next_run).await;
        assert_eq!(due.len(), 1);

        // Paused jobs never fire.
        sched.set_enabled(job.id, false).await.unwrap();
        assert!(sched.due_jobs(job.next_run + 10_000).await.is_empty());
    }

    #[tokio::test]
    async fn mark_fired_skips_past_deadlines() {
        let (sched, _dir) = temp_scheduler();
        let job = sched
            .add("tick".into(), 60, "telegram".into(), "1".into())
            .await
            .unwrap();

        // Simulate long downtime: next_run far in the past.
        {
            let mut table = sched.state.lock().await;
            table.jobs[0].next_run = now_secs() - 10_000;
        }
        sched.mark_fired(job.id).await.unwrap();
        let jobs = sched.list().await;
        assert!(jobs[0].next_run > now_secs());
    }

    #[tokio::test]
    async fn persistence_survives_reload() {
        let dir = tempfile::tempdir().unwrap();
        {
            let sched = Scheduler::new(dir.path().to_path_buf());
            sched
                .add(
                    "daily digest".into(),
                    86_400,
                    "telegram".into(),
                    "42".into(),
                )
                .await
                .unwrap();
        }
        let reloaded = Scheduler::new(dir.path().to_path_buf());
        let jobs = reloaded.list().await;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].prompt, "daily digest");
        assert_eq!(jobs[0].channel_id, "42");
    }
}
