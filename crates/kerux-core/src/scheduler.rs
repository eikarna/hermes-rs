//! Cron/scheduler (F5).
//!
//! User-defined recurring prompts and scheduled agent tasks.
//! Jobs persist to `~/.kerux/scheduler.json` (atomic writes
//! via [`crate::persist`]) and survive restarts.
//!
//! Supports:
//! - Standard 5-field cron expressions: `minute hour day month weekday`
//!   (e.g. `*/5 * * * *`, `0 9 * * 1-5`, `15,45 6,18 * * *`).
//! - One-shot timestamps (ISO-8601 or unix timestamps): `once <timestamp> <prompt>`
//! - Backward-compatible interval syntax: `<number><unit>` pairs (e.g. `30m`, `2h`, `1d`, `1h30m`).
//! - Full agent execution / synthetic agent runs.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::persist;

/// Minimum allowed interval (guards against `1s` hot loops).
pub const MIN_INTERVAL_SECS: u64 = 60;

/// Schedule definition for a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Schedule {
    /// Interval in seconds.
    Interval { seconds: u64 },
    /// 5-field cron expression.
    Cron { expression: String },
    /// One-shot execution at unix timestamp in seconds.
    Once { at: i64 },
}

/// Target execution mode for a scheduled job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum JobTarget {
    /// Send prompt as an incoming message to the platform/channel.
    #[default]
    Prompt,
    /// Trigger a full autonomous/agent execution run.
    Agent,
}

/// One scheduled job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    /// Stable numeric id (shown in `/cron list`, used by `/cron remove`).
    pub id: u64,
    /// The prompt or task sent to the agent/channel on each fire.
    pub prompt: String,
    /// Backward-compatibility helper or interval representation in seconds.
    #[serde(default)]
    pub interval_secs: u64,
    /// Structured schedule specification.
    #[serde(default = "default_interval_schedule")]
    pub schedule: Schedule,
    /// Target mode for the job execution.
    #[serde(default)]
    pub target: JobTarget,
    /// Platform the job delivers to (e.g. "telegram").
    pub platform: String,
    /// Channel/chat id the job delivers to.
    pub channel_id: String,
    /// Unix timestamp of the next scheduled fire.
    pub next_run: i64,
    /// Paused jobs stay stored but never fire.
    pub enabled: bool,
}

fn default_interval_schedule() -> Schedule {
    Schedule::Interval {
        seconds: MIN_INTERVAL_SECS,
    }
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
    path: PathBuf,
}

impl Scheduler {
    /// Construct a scheduler pointing to a directory or a direct JSON file path.
    pub fn new(path_or_dir: PathBuf) -> Self {
        let path = if path_or_dir.is_dir()
            || path_or_dir.extension().is_none()
            || path_or_dir.file_name().and_then(|s| s.to_str()) == Some("cron")
        {
            // If passed a directory or legacy cron dir, resolve to scheduler.json (or fallback jobs.json)
            let candidate = path_or_dir.join("scheduler.json");
            if candidate.is_file() {
                candidate
            } else if path_or_dir.join("jobs.json").is_file() {
                path_or_dir.join("jobs.json")
            } else {
                path_or_dir.join("scheduler.json")
            }
        } else {
            path_or_dir
        };

        let mut state = persist::read_json::<JobTable>(&path).unwrap_or_default();
        // Normalize loaded jobs (e.g. legacy jobs without schedule field)
        for job in &mut state.jobs {
            if matches!(job.schedule, Schedule::Interval { seconds: 60 }) && job.interval_secs > 0 {
                job.schedule = Schedule::Interval {
                    seconds: job.interval_secs,
                };
            } else if let Schedule::Interval { seconds } = job.schedule {
                job.interval_secs = seconds;
            }
        }

        Self {
            state: tokio::sync::Mutex::new(state),
            path,
        }
    }

    /// Default file path: `~/.kerux/scheduler.json`.
    pub fn default_path() -> PathBuf {
        persist::data_root().join("scheduler.json")
    }

    /// Backward compatibility directory helper.
    pub fn default_dir() -> PathBuf {
        persist::data_root()
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn save(&self, table: &JobTable) -> Result<()> {
        persist::write_json(self.path(), table)
            .map_err(|e| Error::Agent(format!("Failed to persist scheduler jobs: {}", e)))
    }

    /// Add a job firing according to `schedule` into `platform:channel_id`.
    pub async fn add_schedule(
        &self,
        prompt: String,
        schedule: Schedule,
        target: JobTarget,
        platform: String,
        channel_id: String,
    ) -> Result<CronJob> {
        self.add_at(prompt, schedule, target, platform, channel_id, now_secs())
            .await
    }

    /// Add a job with an explicit base timestamp for scheduling (useful in tests).
    pub async fn add_at(
        &self,
        prompt: String,
        schedule: Schedule,
        target: JobTarget,
        platform: String,
        channel_id: String,
        now: i64,
    ) -> Result<CronJob> {
        if prompt.trim().is_empty() {
            return Err(Error::Config("Job prompt must not be empty".to_string()));
        }

        let (interval_secs, next_run) = match &schedule {
            Schedule::Interval { seconds } => {
                if *seconds < MIN_INTERVAL_SECS {
                    return Err(Error::Config(format!(
                        "Interval must be at least {} seconds",
                        MIN_INTERVAL_SECS
                    )));
                }
                (*seconds, now.saturating_add(*seconds as i64))
            }
            Schedule::Cron { expression } => {
                let parsed = CronExpression::parse(expression)?;
                let next = parsed.next_after(now)?;
                (0, next)
            }
            Schedule::Once { at } => {
                if *at <= now {
                    return Err(Error::Config(
                        "One-shot scheduled time must be in the future".to_string(),
                    ));
                }
                (0, *at)
            }
        };

        let mut table = self.state.lock().await;
        let id = table.next_id;
        table.next_id += 1;
        let job = CronJob {
            id,
            prompt,
            interval_secs,
            schedule,
            target,
            platform,
            channel_id,
            next_run,
            enabled: true,
        };
        table.jobs.push(job.clone());
        self.save(&table)?;
        Ok(job)
    }

    /// Add a job firing every `interval_secs` into `platform:channel_id` (backward compatibility).
    pub async fn add(
        &self,
        prompt: String,
        interval_secs: u64,
        platform: String,
        channel_id: String,
    ) -> Result<CronJob> {
        self.add_schedule(
            prompt,
            Schedule::Interval {
                seconds: interval_secs,
            },
            JobTarget::Prompt,
            platform,
            channel_id,
        )
        .await
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
        let now = now_secs();
        for job in &mut table.jobs {
            if job.id == id {
                job.enabled = enabled;
                if enabled {
                    // Resuming recalculates next run from now.
                    match &job.schedule {
                        Schedule::Interval { seconds } => {
                            job.next_run = now.saturating_add(*seconds as i64);
                        }
                        Schedule::Cron { expression } => {
                            if let Ok(cron) = CronExpression::parse(expression) {
                                if let Ok(next) = cron.next_after(now) {
                                    job.next_run = next;
                                }
                            }
                        }
                        Schedule::Once { at } => {
                            job.next_run = *at;
                        }
                    }
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

    /// Push a job's next fire forward, or remove it if it was a one-shot job.
    pub async fn mark_fired(&self, id: u64) -> Result<()> {
        self.mark_fired_at(id, now_secs()).await
    }

    /// Mark fired with an explicit current timestamp (useful for testing).
    pub async fn mark_fired_at(&self, id: u64, now: i64) -> Result<()> {
        let mut table = self.state.lock().await;
        let mut to_remove = None;

        for (idx, job) in table.jobs.iter_mut().enumerate() {
            if job.id == id {
                match &job.schedule {
                    Schedule::Interval { seconds } => {
                        let mut next = job.next_run + *seconds as i64;
                        while next <= now {
                            next += *seconds as i64;
                        }
                        job.next_run = next;
                    }
                    Schedule::Cron { expression } => {
                        let parsed = CronExpression::parse(expression)?;
                        job.next_run = parsed.next_after(now)?;
                    }
                    Schedule::Once { .. } => {
                        to_remove = Some(idx);
                    }
                }
                break;
            }
        }

        if let Some(idx) = to_remove {
            table.jobs.remove(idx);
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

/// Parse ISO-8601 UTC timestamp string like `2026-08-28T19:30:00Z` or `2026-08-28 19:30:00` or raw epoch seconds.
pub fn parse_timestamp(input: &str) -> Result<i64> {
    let input = input.trim();
    if input.is_empty() {
        return Err(Error::Config("Empty timestamp".to_string()));
    }

    if let Ok(epoch) = input.parse::<i64>() {
        return Ok(epoch);
    }

    // Parse YYYY-MM-DD[T| ]HH:MM[:SS][Z]
    let s = input.trim_end_matches('Z').trim();
    let parts: Vec<&str> = s.split(['T', ' ']).collect();
    if parts.len() != 2 {
        return Err(Error::Config(format!(
            "Invalid timestamp format '{}', expected YYYY-MM-DDTHH:MM:SSZ or unix epoch",
            input
        )));
    }

    let date_parts: Vec<&str> = parts[0].split('-').collect();
    let time_parts: Vec<&str> = parts[1].split(':').collect();

    if date_parts.len() != 3 || (time_parts.len() != 2 && time_parts.len() != 3) {
        return Err(Error::Config(format!(
            "Invalid timestamp components '{}'",
            input
        )));
    }

    let year: i32 = date_parts[0]
        .parse()
        .map_err(|_| Error::Config("Invalid year".to_string()))?;
    let month: u32 = date_parts[1]
        .parse()
        .map_err(|_| Error::Config("Invalid month".to_string()))?;
    let day: u32 = date_parts[2]
        .parse()
        .map_err(|_| Error::Config("Invalid day".to_string()))?;

    let hour: u32 = time_parts[0]
        .parse()
        .map_err(|_| Error::Config("Invalid hour".to_string()))?;
    let minute: u32 = time_parts[1]
        .parse()
        .map_err(|_| Error::Config("Invalid minute".to_string()))?;
    let second: u32 = if time_parts.len() == 3 {
        time_parts[2]
            .parse()
            .map_err(|_| Error::Config("Invalid second".to_string()))?
    } else {
        0
    };

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(Error::Config("Date out of range".to_string()));
    }
    if hour >= 24 || minute >= 60 || second >= 60 {
        return Err(Error::Config("Time out of range".to_string()));
    }

    let days = days_from_civil(year, month, day);
    let total_secs = days * 86_400 + (hour as i64) * 3600 + (minute as i64) * 60 + (second as i64);
    Ok(total_secs)
}

/// Convert civil date to unix days since 1970-01-01 (standard algorithm).
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + (d as i64) - 1;
    let doe = yoe as i64 * 365 + (yoe as i64) / 4 - (yoe as i64) / 100 + doy;
    era * 146097 + doe - 719468
}

/// Convert unix timestamp in seconds to (year, month, day, hour, minute, second, weekday).
/// Weekday: 0 = Sunday, 1 = Monday, ..., 6 = Saturday (matching standard cron conventions).
pub fn civil_from_timestamp(secs: i64) -> (i32, u32, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem_secs = secs.rem_euclid(86_400) as u32;

    let hour = rem_secs / 3600;
    let minute = (rem_secs % 3600) / 60;
    let second = rem_secs % 60;

    // 1970-01-01 was a Thursday (weekday 4).
    let weekday = ((days + 4).rem_euclid(7)) as u32;

    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    (y as i32, m, d, hour, minute, second, weekday)
}

/// 5-field cron expression parser (minute, hour, day-of-month, month, day-of-week).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronExpression {
    pub minutes: BTreeSet<u32>,
    pub hours: BTreeSet<u32>,
    pub days: BTreeSet<u32>,
    pub months: BTreeSet<u32>,
    pub weekdays: BTreeSet<u32>,
}

impl CronExpression {
    /// Parse standard 5-field cron expression.
    pub fn parse(expr: &str) -> Result<Self> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(Error::Config(format!(
                "Invalid cron expression '{}': expected 5 fields (minute hour day month weekday)",
                expr
            )));
        }

        let minutes = parse_cron_field(fields[0], 0, 59)?;
        let hours = parse_cron_field(fields[1], 0, 23)?;
        let days = parse_cron_field(fields[2], 1, 31)?;
        let months = parse_cron_field(fields[3], 1, 12)?;
        let weekdays = parse_cron_field(fields[4], 0, 7)?; // 0 and 7 are both Sunday

        // Normalize 7 to 0 for Sunday
        let mut normalized_weekdays = BTreeSet::new();
        for mut w in weekdays {
            if w == 7 {
                w = 0;
            }
            normalized_weekdays.insert(w);
        }

        Ok(Self {
            minutes,
            hours,
            days,
            months,
            weekdays: normalized_weekdays,
        })
    }

    /// Check if a given unix timestamp matches the cron expression (down to minute precision).
    pub fn matches_timestamp(&self, secs: i64) -> bool {
        let (_y, m, d, h, min, _s, wday) = civil_from_timestamp(secs);
        self.minutes.contains(&min)
            && self.hours.contains(&h)
            && self.days.contains(&d)
            && self.months.contains(&m)
            && self.weekdays.contains(&wday)
    }

    /// Find the next scheduled execution timestamp strictly after `now_secs`.
    pub fn next_after(&self, now_secs: i64) -> Result<i64> {
        // Start search from next minute boundary
        let current_minute = (now_secs / 60) * 60;
        let mut candidate = current_minute + 60;

        // Search up to 5 years forward (5 * 366 * 1440 minutes ~ 2.6M minutes)
        let max_search_minutes = 5 * 366 * 1440;
        for _ in 0..max_search_minutes {
            if self.matches_timestamp(candidate) {
                return Ok(candidate);
            }
            candidate += 60;
        }

        Err(Error::Config(
            "Could not find next matching time for cron expression within 5 years".to_string(),
        ))
    }
}

/// Parse a single cron field with ranges, lists, and steps.
fn parse_cron_field(field: &str, min: u32, max: u32) -> Result<BTreeSet<u32>> {
    let field = field.trim();
    if field.is_empty() {
        return Err(Error::Config("Empty cron field".to_string()));
    }

    let mut values = BTreeSet::new();
    for part in field.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(Error::Config("Empty element in cron list".to_string()));
        }

        let (range_str, step) = if let Some((r, s)) = part.split_once('/') {
            let step_val: u32 = s
                .parse()
                .map_err(|_| Error::Config(format!("Invalid cron step in '{}'", part)))?;
            if step_val == 0 {
                return Err(Error::Config("Cron step must be > 0".to_string()));
            }
            (r, step_val)
        } else {
            (part, 1)
        };

        let (start, end) = if range_str == "*" {
            (min, max)
        } else if let Some((start_s, end_s)) = range_str.split_once('-') {
            let start_v: u32 = start_s
                .parse()
                .map_err(|_| Error::Config(format!("Invalid range start in '{}'", part)))?;
            let end_v: u32 = end_s
                .parse()
                .map_err(|_| Error::Config(format!("Invalid range end in '{}'", part)))?;
            if start_v > end_v {
                return Err(Error::Config(format!(
                    "Range start {} > end {} in '{}'",
                    start_v, end_v, part
                )));
            }
            (start_v, end_v)
        } else {
            let single_v: u32 = range_str
                .parse()
                .map_err(|_| Error::Config(format!("Invalid cron value '{}'", part)))?;
            (single_v, single_v)
        };

        if start < min || end > max {
            return Err(Error::Config(format!(
                "Cron value out of range [{}, {}] in '{}'",
                min, max, part
            )));
        }

        let mut curr = start;
        while curr <= end {
            values.insert(curr);
            curr = match curr.checked_add(step) {
                Some(next) => next,
                None => break,
            };
        }
    }

    if values.is_empty() {
        return Err(Error::Config(format!(
            "No valid values produced for cron field '{}'",
            field
        )));
    }

    Ok(values)
}

/// Helper to parse add request syntax from CLI or chat.
/// Supports:
/// - `<interval> <prompt>` (e.g. `30m check inbox`)
/// - `cron "<expr>" <prompt>` or `cron <5-fields> <prompt>`
/// - `once <timestamp> <prompt>` or `at <timestamp> <prompt>`
pub fn parse_add_request(input: &str) -> Result<(Schedule, String)> {
    let input = input.trim();
    if input.is_empty() {
        return Err(Error::Config("Empty add request".to_string()));
    }

    if let Some(rest) = input.strip_prefix("cron ") {
        let rest = rest.trim();
        if let Some(stripped) = rest.strip_prefix('"') {
            if let Some(end_quote) = stripped.find('"') {
                let expr = &stripped[..end_quote];
                let prompt = stripped[end_quote + 1..].trim();
                if prompt.is_empty() {
                    return Err(Error::Config(
                        "Missing prompt after cron expression".to_string(),
                    ));
                }
                let _ = CronExpression::parse(expr)?;
                return Ok((
                    Schedule::Cron {
                        expression: expr.to_string(),
                    },
                    prompt.to_string(),
                ));
            }
        }
        // Try 5 whitespace fields
        let tokens: Vec<&str> = rest.split_whitespace().collect();
        if tokens.len() >= 6 {
            let expr = tokens[..5].join(" ");
            let prompt = tokens[5..].join(" ");
            let _ = CronExpression::parse(&expr)?;
            return Ok((Schedule::Cron { expression: expr }, prompt));
        }
        return Err(Error::Config(
            "Invalid cron add syntax. Use: cron \"*/5 * * * *\" <prompt>".to_string(),
        ));
    }

    if let Some(rest) = input
        .strip_prefix("once ")
        .or_else(|| input.strip_prefix("at "))
    {
        let rest = rest.trim();
        let mut parts = rest.splitn(2, ' ');
        let ts_str = parts.next().unwrap_or("");
        let prompt = parts.next().unwrap_or("").trim();
        if ts_str.is_empty() || prompt.is_empty() {
            return Err(Error::Config(
                "Usage: once <timestamp> <prompt>".to_string(),
            ));
        }
        let at = parse_timestamp(ts_str)?;
        return Ok((Schedule::Once { at }, prompt.to_string()));
    }

    // Default to interval syntax
    let mut parts = input.splitn(2, ' ');
    let interval_str = parts.next().unwrap_or("");
    let prompt = parts.next().unwrap_or("").trim();
    if interval_str.is_empty() || prompt.is_empty() {
        return Err(Error::Config(
            "Usage: /cron add <interval|cron|once> <prompt>".to_string(),
        ));
    }
    let seconds = parse_interval(interval_str)?;
    Ok((Schedule::Interval { seconds }, prompt.to_string()))
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

    #[test]
    fn cron_parser_supports_steps_ranges_and_lists() {
        let every_five = CronExpression::parse("*/5 * * * *").unwrap();
        assert!(every_five.matches_timestamp(1_788_000_000));
        assert!(!every_five.matches_timestamp(1_788_000_060));

        let ranged = CronExpression::parse("0 9-17 * * 1-5").unwrap();
        assert!(ranged.matches_timestamp(parse_timestamp("2026-08-28T09:00:00Z").unwrap()));
        assert!(!ranged.matches_timestamp(parse_timestamp("2026-08-29T09:00:00Z").unwrap()));

        let listed = CronExpression::parse("15,45 6,18 * * *").unwrap();
        assert!(listed.matches_timestamp(parse_timestamp("2026-08-28T18:45:00Z").unwrap()));
        assert!(!listed.matches_timestamp(parse_timestamp("2026-08-28T18:30:00Z").unwrap()));
    }

    #[test]
    fn cron_parser_rejects_invalid_fields() {
        for value in [
            "* * * *",
            "*/0 * * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * * 13 *",
            "* * * * 8",
            "10-5 * * * *",
            "1,,2 * * * *",
        ] {
            assert!(CronExpression::parse(value).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn cron_next_run_is_strictly_after_current_minute() {
        let cron = CronExpression::parse("*/5 * * * *").unwrap();
        let now = parse_timestamp("2026-08-28T10:10:00Z").unwrap();
        assert_eq!(
            cron.next_after(now).unwrap(),
            parse_timestamp("2026-08-28T10:15:00Z").unwrap()
        );
    }

    #[test]
    fn add_request_parses_interval_cron_and_one_shot() {
        let (schedule, prompt) = parse_add_request("30m check the news").unwrap();
        assert_eq!(schedule, Schedule::Interval { seconds: 1_800 });
        assert_eq!(prompt, "check the news");

        let (schedule, prompt) =
            parse_add_request("cron \"*/5 9-17 * * 1-5\" check inbox").unwrap();
        assert_eq!(
            schedule,
            Schedule::Cron {
                expression: "*/5 9-17 * * 1-5".into()
            }
        );
        assert_eq!(prompt, "check inbox");

        let (schedule, prompt) =
            parse_add_request("once 2026-08-28T19:30:00Z prepare report").unwrap();
        assert_eq!(
            schedule,
            Schedule::Once {
                at: parse_timestamp("2026-08-28T19:30:00Z").unwrap()
            }
        );
        assert_eq!(prompt, "prepare report");
    }

    #[tokio::test]
    async fn one_shot_is_removed_after_firing() {
        let (sched, _dir) = temp_scheduler();
        let now = parse_timestamp("2026-08-28T10:00:00Z").unwrap();
        let job = sched
            .add_at(
                "run report".into(),
                Schedule::Once { at: now + 60 },
                JobTarget::Prompt,
                "telegram".into(),
                "42".into(),
                now,
            )
            .await
            .unwrap();

        assert!(sched.due_jobs(now + 59).await.is_empty());
        assert_eq!(sched.due_jobs(now + 60).await.len(), 1);
        sched.mark_fired_at(job.id, now + 60).await.unwrap();
        assert!(sched.list().await.is_empty());
    }

    #[tokio::test]
    async fn cron_job_advances_to_next_matching_minute() {
        let (sched, _dir) = temp_scheduler();
        let now = parse_timestamp("2026-08-28T10:01:00Z").unwrap();
        let job = sched
            .add_at(
                "poll".into(),
                Schedule::Cron {
                    expression: "*/5 * * * *".into(),
                },
                JobTarget::Prompt,
                "telegram".into(),
                "42".into(),
                now,
            )
            .await
            .unwrap();
        assert_eq!(
            job.next_run,
            parse_timestamp("2026-08-28T10:05:00Z").unwrap()
        );

        sched.mark_fired_at(job.id, job.next_run).await.unwrap();
        let jobs = sched.list().await;
        assert_eq!(
            jobs[0].next_run,
            parse_timestamp("2026-08-28T10:10:00Z").unwrap()
        );
    }

    #[tokio::test]
    async fn scheduler_uses_single_scheduler_json_file() {
        let dir = tempfile::tempdir().unwrap();
        let sched = Scheduler::new(dir.path().to_path_buf());
        sched
            .add("persist me".into(), 60, "telegram".into(), "7".into())
            .await
            .unwrap();
        assert!(dir.path().join("scheduler.json").is_file());
    }
}
