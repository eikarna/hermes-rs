//! Date and time tools
//!
//! Tools for getting current time and timestamps.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::schema::ToolSchema;
use crate::tools::{HermesTool, ToolContext, ToolResult};

/// Tool for getting current date and time
pub struct DateTimeTool;

#[derive(JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DateTimeArgs {
    /// Timezone offset in hours from UTC (e.g., -5, +9, +5.5) or timezone name like "UTC", "EST", "PST"
    timezone: Option<String>,
    /// strftime-style format string (e.g., "%Y-%m-%d %H:%M:%S")
    format: Option<String>,
}

#[async_trait]
impl HermesTool for DateTimeTool {
    fn name(&self) -> &str {
        "datetime"
    }

    fn description(&self) -> &str {
        "Get the current date and time. Supports timezone offsets (e.g., '+9', '-5', '+5.5') \
        or common abbreviations ('UTC', 'EST', 'PST', 'JST', etc.) and custom strftime formatting."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::from_type::<DateTimeArgs>("datetime", "Get current date and time")
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let args: DateTimeArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error("datetime", format!("Invalid arguments: {}", e)),
        };

        let now = SystemTime::now();
        let duration = now.duration_since(UNIX_EPOCH).unwrap_or_default();
        let utc_secs = duration.as_secs();
        let nsecs = duration.subsec_nanos();

        // Parse timezone offset
        let tz_str = args.timezone.unwrap_or_else(|| "UTC".to_string());
        let offset_secs = parse_timezone_offset(&tz_str);
        let local_secs = (utc_secs as i64 + offset_secs) as u64;

        let tz_label = if offset_secs == 0 {
            "UTC".to_string()
        } else {
            let hours = offset_secs / 3600;
            let mins = (offset_secs.abs() % 3600) / 60;
            if mins == 0 {
                format!("UTC{:+}", hours)
            } else {
                format!("UTC{:+}:{:02}", hours, mins)
            }
        };

        let format = args
            .format
            .unwrap_or_else(|| "%Y-%m-%d %H:%M:%S".to_string());
        let dt = format_datetime(local_secs, nsecs, &format);

        ToolResult::success(
            "datetime",
            serde_json::json!({
                "timestamp": utc_secs,
                "nanoseconds": nsecs,
                "formatted": dt,
                "timezone": tz_label,
                "timezone_offset_seconds": offset_secs,
                "unix_timestamp": utc_secs
            }),
        )
    }
}

/// Tool for getting Unix timestamps
pub struct TimestampTool;

#[derive(JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TimestampArgs {
    date: Option<String>,
    unit: Option<String>,
}

#[async_trait]
impl HermesTool for TimestampTool {
    fn name(&self) -> &str {
        "timestamp"
    }

    fn description(&self) -> &str {
        "Get the current Unix timestamp or convert a date string to a timestamp. \
        Use this when you need precise timing information or need to calculate time differences."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::from_type::<TimestampArgs>("timestamp", "Get Unix timestamp")
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let args: TimestampArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error("timestamp", format!("Invalid arguments: {}", e)),
        };

        let now = SystemTime::now();
        let duration = now.duration_since(UNIX_EPOCH).unwrap_or_default();

        let (timestamp, unit) = if let Some(ref date) = args.date {
            // Parse date string - simplified, supports ISO 8601 and common formats
            match parse_date(date) {
                Ok(ts) => (ts, args.unit.unwrap_or_else(|| "seconds".to_string())),
                Err(e) => return ToolResult::error("timestamp", e),
            }
        } else {
            let unit = args.unit.unwrap_or_else(|| "seconds".to_string());
            let ts = match unit.as_str() {
                "milliseconds" | "ms" => duration.as_millis() as u64,
                "microseconds" | "us" => duration.as_micros() as u64,
                "nanoseconds" | "ns" => duration.as_nanos() as u64,
                _ => duration.as_secs(),
            };
            (ts, unit)
        };

        ToolResult::success(
            "timestamp",
            serde_json::json!({
                "timestamp": timestamp,
                "unit": unit,
                "datetime": format_datetime(timestamp, 0, "%Y-%m-%d %H:%M:%S"),
                "iso8601": format_datetime(timestamp, 0, "%Y-%m-%dT%H:%M:%SZ")
            }),
        )
    }
}

/// Simple datetime formatter (no external dependencies)
fn format_datetime(secs: u64, nsecs: u32, format: &str) -> String {
    // Calculate date/time components from Unix timestamp
    let days = secs / 86400;
    let mut remaining_secs = secs % 86400;
    let hours = remaining_secs / 3600;
    remaining_secs %= 3600;
    let minutes = remaining_secs / 60;
    let seconds = remaining_secs % 60;

    // Calculate year, month, day using Zeller's congruence approximation
    let (year, month, day) = days_to_date(days);

    // Format according to pattern
    format
        .replace("%Y", &format!("{:04}", year))
        .replace("%m", &format!("{:02}", month))
        .replace("%d", &format!("{:02}", day))
        .replace("%H", &format!("{:02}", hours))
        .replace("%M", &format!("{:02}", minutes))
        .replace("%S", &format!("{:02}", seconds))
        .replace("%f", &format!("{:09}", nsecs))
        .replace("%T", &format!("{:02}:{:02}:{:02}", hours, minutes, seconds))
}

/// Convert days since epoch to year, month, day
fn days_to_date(days: u64) -> (u64, u8, u8) {
    // Simplified algorithm - works for dates after 1970-01-01
    let mut year = 1970;
    let mut remaining_days = days as i64;

    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        year += 1;
    }

    // Days in each month (non-leap year)
    let days_in_months = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 1;
    for (i, &dim) in days_in_months.iter().enumerate() {
        let days_in_this_month = if i == 1 && is_leap_year(year) {
            29
        } else {
            dim
        };
        if remaining_days < days_in_this_month as i64 {
            break;
        }
        remaining_days -= days_in_this_month as i64;
        month = i as u8 + 1;
    }

    (year, month, (remaining_days + 1) as u8)
}

fn is_leap_year(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Parse a date string to Unix timestamp (simplified)
fn parse_date(date: &str) -> Result<u64, String> {
    // Try ISO 8601 format first: YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS
    let parts: Vec<&str> = date
        .split(['-', 'T', ':', 'Z', '+', ' '])
        .collect();

    if parts.len() >= 3 {
        let year: u64 = parts[0].parse().map_err(|_| "Invalid year")?;
        let month: u64 = parts[1].parse().map_err(|_| "Invalid month")?;
        let day: u64 = parts[2].parse().map_err(|_| "Invalid day")?;

        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return Err("Invalid date values".to_string());
        }

        // Calculate days since epoch
        let mut days = 0u64;
        for y in 1970..year {
            days += if is_leap_year(y) { 366 } else { 365 };
        }

        let days_in_months = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        for m in 1..month {
            days += days_in_months[(m - 1) as usize] as u64;
        }
        if is_leap_year(year) && month > 2 {
            days += 1;
        }
        days += day - 1;

        let mut secs = days * 86400;

        // Add time if present
        if parts.len() >= 6 {
            secs += parts[3].parse::<u64>().unwrap_or(0) * 3600;
            secs += parts[4].parse::<u64>().unwrap_or(0) * 60;
            secs += parts[5].parse::<u64>().unwrap_or(0);
        }

        return Ok(secs);
    }

    Err(format!("Unable to parse date: {}", date))
}

/// Parse a timezone string to offset in seconds from UTC.
///
/// Supports:
/// - Numeric offsets: "+9", "-5", "+5.5", "+05:30"
/// - Common abbreviations: "UTC", "EST", "CST", "MST", "PST", "JST", "CET", "EET", "AEST", etc.
fn parse_timezone_offset(tz: &str) -> i64 {
    let tz = tz.trim();

    // Try numeric offset first
    if tz.starts_with('+') || tz.starts_with('-') {
        if let Some(colon_pos) = tz.find(':') {
            // Format: +HH:MM or -HH:MM
            let hours: i64 = tz[..colon_pos].parse().unwrap_or(0);
            let mins: i64 = tz[colon_pos + 1..].parse().unwrap_or(0);
            let sign = if hours < 0 { -1 } else { 1 };
            return hours * 3600 + sign * mins * 60;
        }
        // Format: +N or +N.N
        if let Ok(hours) = tz.parse::<f64>() {
            return (hours * 3600.0) as i64;
        }
    }

    // Try bare number (offset without sign)
    if let Ok(hours) = tz.parse::<f64>() {
        return (hours * 3600.0) as i64;
    }

    // Common timezone abbreviations
    match tz.to_uppercase().as_str() {
        "UTC" | "GMT" | "Z" => 0,
        "EST" => -5 * 3600,
        "EDT" => -4 * 3600,
        "CST" => -6 * 3600,
        "CDT" => -5 * 3600,
        "MST" => -7 * 3600,
        "MDT" => -6 * 3600,
        "PST" => -8 * 3600,
        "PDT" => -7 * 3600,
        "AKST" => -9 * 3600,
        "AKDT" => -8 * 3600,
        "HST" => -10 * 3600,
        "JST" => 9 * 3600,
        "KST" => 9 * 3600,
        "CST_CHINA" | "CCT" => 8 * 3600,
        "HKT" => 8 * 3600,
        "SGT" => 8 * 3600,
        "IST" => 5 * 3600 + 1800, // India +5:30
        "ICT" => 7 * 3600,        // Indochina (Thailand, Vietnam)
        "WIB" => 7 * 3600,        // Western Indonesia
        "CET" => 3600,
        "CEST" => 2 * 3600,
        "EET" => 2 * 3600,
        "EEST" => 3 * 3600,
        "MSK" => 3 * 3600,
        "GST" => 4 * 3600, // Gulf Standard Time
        "PKT" => 5 * 3600,
        "BST" => 3600, // British Summer Time
        "AEST" => 10 * 3600,
        "AEDT" => 11 * 3600,
        "ACST" => 9 * 3600 + 1800, // Australian Central +9:30
        "AWST" => 8 * 3600,
        "NZST" => 12 * 3600,
        "NZDT" => 13 * 3600,
        "BRT" => -3 * 3600,
        "ART" => -3 * 3600,
        "CLT" => -4 * 3600,
        "COT" => -5 * 3600,
        "PET" => -5 * 3600,
        "VET" => -4 * 3600,
        "ECT" => -5 * 3600,
        _ => 0, // Unknown timezone, default to UTC
    }
}
