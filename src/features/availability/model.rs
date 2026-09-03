//! Weekly availability slots + one-off exceptions, and their file I/O
//! (PLAN.md §4). Partitioned by `vet_id`, same as appointments/locks — a
//! read or write for one vet never touches another vet's directory.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::{Date, Time};
use uuid::Uuid;

use crate::storage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DayOfWeek {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvailabilitySlot {
    pub id: Uuid,
    pub vet_id: Uuid,
    pub day_of_week: DayOfWeek,
    pub start_time: Time,
    pub end_time: Time,
}

/// A one-off override for a specific date — a full day off
/// (`is_available: false`, no times) or custom hours for that date
/// (`is_available: true`, both times set), replacing the recurring weekly
/// schedule for that one day only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvailabilityException {
    pub id: Uuid,
    pub vet_id: Uuid,
    pub date: Date,
    pub is_available: bool,
    pub start_time: Option<Time>,
    pub end_time: Option<Time>,
}

fn availability_dir(data_dir: &Path, vet_id: Uuid) -> PathBuf {
    data_dir.join("availability").join(vet_id.to_string())
}

fn availability_path(data_dir: &Path, vet_id: Uuid, id: Uuid) -> PathBuf {
    availability_dir(data_dir, vet_id).join(format!("{id}.json"))
}

fn exceptions_dir(data_dir: &Path, vet_id: Uuid) -> PathBuf {
    data_dir
        .join("availability_exceptions")
        .join(vet_id.to_string())
}

fn exception_path(data_dir: &Path, vet_id: Uuid, id: Uuid) -> PathBuf {
    exceptions_dir(data_dir, vet_id).join(format!("{id}.json"))
}

/// The registration slice's `users.lock` counterpart for this slice: a vet
/// editing their own weekly schedule or exceptions (PLAN.md §3).
pub fn lock_path(data_dir: &Path, vet_id: Uuid) -> PathBuf {
    data_dir
        .join("locks")
        .join(format!("availability-{vet_id}.lock"))
}

pub fn write_slot(data_dir: &Path, slot: &AvailabilitySlot) -> io::Result<()> {
    storage::atomic_write(&availability_path(data_dir, slot.vet_id, slot.id), slot)
}

/// Cross-slice read: `appointments` calls this to derive free slots
/// (PLAN.md §6 constraint 5).
pub fn read_weekly(data_dir: &Path, vet_id: Uuid) -> io::Result<Vec<AvailabilitySlot>> {
    storage::list_dir_json(&availability_dir(data_dir, vet_id))
}

/// Full replace of a vet's weekly schedule — deletes every existing slot
/// file for `vet_id` first. Must be called holding the vet's exclusive
/// availability lock, same as the write that follows it, so a concurrent
/// reader never observes the directory between "old slots gone" and "new
/// slots written" (PLAN.md §5).
pub fn delete_all_slots(data_dir: &Path, vet_id: Uuid) -> io::Result<()> {
    match std::fs::remove_dir_all(availability_dir(data_dir, vet_id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

pub fn write_exception(data_dir: &Path, exception: &AvailabilityException) -> io::Result<()> {
    storage::atomic_write(
        &exception_path(data_dir, exception.vet_id, exception.id),
        exception,
    )
}

/// Cross-slice read: `appointments` calls this to derive free slots for one
/// day (PLAN.md §6 constraint 5).
pub fn read_exceptions(data_dir: &Path, vet_id: Uuid) -> io::Result<Vec<AvailabilityException>> {
    storage::list_dir_json(&exceptions_dir(data_dir, vet_id))
}

/// At most one exception per vet per date — this is what makes setting an
/// exception an upsert rather than an ever-growing list.
pub fn find_exception_by_date(
    data_dir: &Path,
    vet_id: Uuid,
    date: Date,
) -> io::Result<Option<AvailabilityException>> {
    let exceptions = read_exceptions(data_dir, vet_id)?;
    Ok(exceptions.into_iter().find(|e| e.date == date))
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::{date, time};

    fn sample_slot(vet_id: Uuid) -> AvailabilitySlot {
        AvailabilitySlot {
            id: Uuid::new_v4(),
            vet_id,
            day_of_week: DayOfWeek::Monday,
            start_time: time!(9:00),
            end_time: time!(12:00),
        }
    }

    fn sample_exception(vet_id: Uuid, date: Date) -> AvailabilityException {
        AvailabilityException {
            id: Uuid::new_v4(),
            vet_id,
            date,
            is_available: false,
            start_time: None,
            end_time: None,
        }
    }

    #[test]
    fn write_slot_then_read_weekly_returns_it() {
        let dir = tempfile::tempdir().unwrap();
        let vet_id = Uuid::new_v4();
        let slot = sample_slot(vet_id);

        write_slot(dir.path(), &slot).unwrap();

        assert_eq!(read_weekly(dir.path(), vet_id).unwrap(), vec![slot]);
    }

    #[test]
    fn read_weekly_only_returns_that_vets_slots() {
        let dir = tempfile::tempdir().unwrap();
        let vet_a = Uuid::new_v4();
        let vet_b = Uuid::new_v4();
        write_slot(dir.path(), &sample_slot(vet_a)).unwrap();
        write_slot(dir.path(), &sample_slot(vet_b)).unwrap();

        assert_eq!(read_weekly(dir.path(), vet_a).unwrap().len(), 1);
    }

    #[test]
    fn read_weekly_with_no_slots_returns_empty() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(read_weekly(dir.path(), Uuid::new_v4()).unwrap(), Vec::new());
    }

    #[test]
    fn delete_all_slots_clears_a_vets_weekly_schedule() {
        let dir = tempfile::tempdir().unwrap();
        let vet_id = Uuid::new_v4();
        write_slot(dir.path(), &sample_slot(vet_id)).unwrap();

        delete_all_slots(dir.path(), vet_id).unwrap();

        assert_eq!(read_weekly(dir.path(), vet_id).unwrap(), Vec::new());
    }

    #[test]
    fn delete_all_slots_on_a_vet_with_none_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();

        delete_all_slots(dir.path(), Uuid::new_v4()).unwrap();
    }

    #[test]
    fn write_exception_then_find_by_date_matches() {
        let dir = tempfile::tempdir().unwrap();
        let vet_id = Uuid::new_v4();
        let exception = sample_exception(vet_id, date!(2026 - 09 - 03));
        write_exception(dir.path(), &exception).unwrap();

        assert_eq!(
            find_exception_by_date(dir.path(), vet_id, date!(2026 - 09 - 03)).unwrap(),
            Some(exception)
        );
        assert_eq!(
            find_exception_by_date(dir.path(), vet_id, date!(2026 - 09 - 04)).unwrap(),
            None
        );
    }
}
