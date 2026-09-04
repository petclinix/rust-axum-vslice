//! Append-only activity log. `record` is the one function
//! other slices call into — login, booking created/cancelled/etc. — "a
//! thin, one-directional dependency on a logging utility... not a shared
//! business-logic layer" (see `docs/architecture.md`'s Slice Responsibilities).

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};

use crate::auth::AuthUser;
use crate::config::Config;
use crate::domain::Role;
use crate::error::AppError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityEvent {
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    pub event_type: String,
    pub details: serde_json::Value,
}

fn activity_log_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("activity_log")
}

/// Reuses the crate's already-established human-readable `"YYYY-MM-DD"`
/// `Date` encoding (confirmed via `serde_json`) rather than hand-rolling a
/// second format string for the same value.
fn date_filename(date: Date) -> String {
    serde_json::to_string(&date)
        .expect("Date always serializes")
        .trim_matches('"')
        .to_string()
}

fn log_path(data_dir: &Path, date: Date) -> PathBuf {
    activity_log_dir(data_dir).join(format!("{}.ndjson", date_filename(date)))
}

/// Appends one line to today's NDJSON file. No lock: each `write()` under
/// `O_APPEND` is atomic at the OS level for a line this size, and this is
/// deliberately a thin logging utility, not a correctness-critical write
/// path the way booking is (`docs/architecture-internals.md` §1) — the
/// same "no invariant to protect" trade-off as other non-critical writes
/// (see `docs/architecture.md`'s Design Constraints).
pub fn record(data_dir: &Path, event_type: &str, details: serde_json::Value) -> io::Result<()> {
    let event = ActivityEvent {
        timestamp: OffsetDateTime::now_utc(),
        event_type: event_type.to_string(),
        details,
    };

    let dir = activity_log_dir(data_dir);
    fs::create_dir_all(&dir)?;
    let line =
        serde_json::to_string(&event).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(data_dir, event.timestamp.date()))?;
    writeln!(file, "{line}")
}

fn parse_ndjson_file(path: &Path) -> io::Result<Vec<ActivityEvent>> {
    match fs::read_to_string(path) {
        Ok(contents) => contents
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
            })
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

pub fn read_for_date(data_dir: &Path, date: Date) -> io::Result<Vec<ActivityEvent>> {
    parse_ndjson_file(&log_path(data_dir, date))
}

pub fn read_all(data_dir: &Path) -> io::Result<Vec<ActivityEvent>> {
    let dir = activity_log_dir(data_dir);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let mut events = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.path().extension().and_then(|ext| ext.to_str()) == Some("ndjson") {
            events.extend(parse_ndjson_file(&entry.path())?);
        }
    }
    events.sort_by_key(|e| e.timestamp);
    Ok(events)
}

#[derive(Debug, Deserialize)]
pub struct ActivityQuery {
    #[serde(default)]
    pub date: Option<Date>,
}

pub async fn list_activity(
    State(config): State<Config>,
    auth: AuthUser,
    Query(query): Query<ActivityQuery>,
) -> Result<Json<Vec<ActivityEvent>>, AppError> {
    if auth.role != Role::Admin {
        return Err(AppError::Forbidden(
            "this endpoint requires the admin role".to_string(),
        ));
    }

    let data_dir = config.data_dir.clone();
    let events = tokio::task::spawn_blocking(move || match query.date {
        Some(date) => read_for_date(&data_dir, date),
        None => read_all(&data_dir),
    })
    .await
    .map_err(|_| AppError::Internal)??;

    Ok(Json(events))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::macros::date;

    #[test]
    fn record_then_read_for_date_round_trips() {
        let dir = tempfile::tempdir().unwrap();

        record(dir.path(), "user_login", json!({"user_id": "u1"})).unwrap();
        record(dir.path(), "appointment_booked", json!({"id": "a1"})).unwrap();

        let today = OffsetDateTime::now_utc().date();
        let events = read_for_date(dir.path(), today).unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "user_login");
        assert_eq!(events[1].event_type, "appointment_booked");
    }

    #[test]
    fn read_for_date_with_no_log_returns_empty() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(
            read_for_date(dir.path(), date!(2020 - 01 - 01)).unwrap(),
            Vec::new()
        );
    }

    #[test]
    fn read_all_spans_every_days_file_in_timestamp_order() {
        let dir = tempfile::tempdir().unwrap();
        // Write directly to two different days' files to avoid depending on
        // real elapsed time between two `record` calls on the same day.
        let yesterday = ActivityEvent {
            timestamp: OffsetDateTime::now_utc() - time::Duration::days(1),
            event_type: "user_login".to_string(),
            details: json!({}),
        };
        let today = ActivityEvent {
            timestamp: OffsetDateTime::now_utc(),
            event_type: "appointment_booked".to_string(),
            details: json!({}),
        };
        for event in [&today, &yesterday] {
            fs::create_dir_all(activity_log_dir(dir.path())).unwrap();
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_path(dir.path(), event.timestamp.date()))
                .unwrap();
            writeln!(file, "{}", serde_json::to_string(event).unwrap()).unwrap();
        }

        let events = read_all(dir.path()).unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "user_login");
        assert_eq!(events[1].event_type, "appointment_booked");
    }

    #[test]
    fn read_all_with_no_directory_returns_empty() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(read_all(dir.path()).unwrap(), Vec::new());
    }
}
