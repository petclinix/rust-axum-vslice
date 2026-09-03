//! Appointment records and their file I/O (PLAN.md §4). Partitioned by
//! `vet_id`, exactly like `availability` — a booking attempt only ever
//! locks and scans one vet's directory (PLAN.md §5).

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::{Duration, PrimitiveDateTime};
use uuid::Uuid;

use crate::domain::{AppointmentStatus, TimeRange};
use crate::storage;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Appointment {
    pub id: Uuid,
    pub pet_id: Uuid,
    pub vet_id: Uuid,
    pub time_slot: PrimitiveDateTime,
    pub duration_minutes: i64,
    pub status: AppointmentStatus,
}

impl Appointment {
    /// `Booked`/`Confirmed` occupy time on the calendar and block new
    /// bookings; `Completed`/`Cancelled`/`NoShow` don't (PLAN.md §5
    /// pseudocode's `read_active_for_vet`).
    pub fn is_active(&self) -> bool {
        matches!(
            self.status,
            AppointmentStatus::Booked | AppointmentStatus::Confirmed
        )
    }

    pub fn time_range(&self) -> TimeRange {
        TimeRange::new(
            self.time_slot,
            self.time_slot + Duration::minutes(self.duration_minutes),
        )
    }
}

fn appointments_root(data_dir: &Path) -> PathBuf {
    data_dir.join("appointments")
}

fn appointments_dir(data_dir: &Path, vet_id: Uuid) -> PathBuf {
    appointments_root(data_dir).join(vet_id.to_string())
}

fn appointment_path(data_dir: &Path, vet_id: Uuid, id: Uuid) -> PathBuf {
    appointments_dir(data_dir, vet_id).join(format!("{id}.json"))
}

/// The headline lock (PLAN.md §5): every write path (book, cancel,
/// reschedule, confirm, complete, no-show) takes this exclusively before
/// touching `vet_id`'s appointments.
pub fn lock_path(data_dir: &Path, vet_id: Uuid) -> PathBuf {
    data_dir.join("locks").join(format!("vet-{vet_id}.lock"))
}

pub fn write_appointment(data_dir: &Path, appointment: &Appointment) -> io::Result<()> {
    storage::atomic_write(
        &appointment_path(data_dir, appointment.vet_id, appointment.id),
        appointment,
    )
}

pub fn read_appointment(
    data_dir: &Path,
    vet_id: Uuid,
    id: Uuid,
) -> io::Result<Option<Appointment>> {
    storage::read_json(&appointment_path(data_dir, vet_id, id))
}

pub fn read_all_for_vet(data_dir: &Path, vet_id: Uuid) -> io::Result<Vec<Appointment>> {
    storage::list_dir_json(&appointments_dir(data_dir, vet_id))
}

pub fn read_active_for_vet(data_dir: &Path, vet_id: Uuid) -> io::Result<Vec<Appointment>> {
    Ok(read_all_for_vet(data_dir, vet_id)?
        .into_iter()
        .filter(Appointment::is_active)
        .collect())
}

/// Locates which vet's directory holds `appointment_id`, for the paths
/// (owner-initiated cancel/reschedule) that only have the appointment id,
/// not the vet id — an owner may have booked with any vet. This checks one
/// filename per vet directory rather than parsing every appointment, so
/// it's a targeted existence check, not the kind of whole-`data/` scan
/// PLAN.md §4 warns against; the on-disk layout has no secondary index from
/// appointment id to vet id, so a bounded scan across vet directories is
/// the only way to resolve one without it. `vet_id` never changes for an
/// appointment once created, so this is safe to do before taking any lock.
pub fn find_vet_id_for_appointment(
    data_dir: &Path,
    appointment_id: Uuid,
) -> io::Result<Option<Uuid>> {
    let root = appointments_root(data_dir);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };

    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Ok(vet_id) = entry.file_name().to_string_lossy().parse::<Uuid>() else {
            continue;
        };
        if entry.path().join(format!("{appointment_id}.json")).exists() {
            return Ok(Some(vet_id));
        }
    }
    Ok(None)
}

/// Every appointment across every vet — used only by the owner-scoped "my
/// appointments" listing, which has no single vet to scope to (an owner's
/// pets may have appointments with several vets). Same rationale as admin
/// stats in PLAN.md §4: a global scan is unavoidable when the query itself
/// is global, not vet-scoped.
pub fn read_all(data_dir: &Path) -> io::Result<Vec<Appointment>> {
    let root = appointments_root(data_dir);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let mut all = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        all.extend(storage::list_dir_json::<Appointment>(&entry.path())?);
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn sample_appointment(vet_id: Uuid, status: AppointmentStatus) -> Appointment {
        Appointment {
            id: Uuid::new_v4(),
            pet_id: Uuid::new_v4(),
            vet_id,
            time_slot: datetime!(2026-09-10 9:00),
            duration_minutes: 30,
            status,
        }
    }

    #[test]
    fn is_active_true_for_booked_and_confirmed_only() {
        assert!(sample_appointment(Uuid::new_v4(), AppointmentStatus::Booked).is_active());
        assert!(sample_appointment(Uuid::new_v4(), AppointmentStatus::Confirmed).is_active());
        assert!(!sample_appointment(Uuid::new_v4(), AppointmentStatus::Completed).is_active());
        assert!(!sample_appointment(Uuid::new_v4(), AppointmentStatus::Cancelled).is_active());
        assert!(!sample_appointment(Uuid::new_v4(), AppointmentStatus::NoShow).is_active());
    }

    #[test]
    fn time_range_spans_the_booked_duration() {
        let appointment = sample_appointment(Uuid::new_v4(), AppointmentStatus::Booked);
        let range = appointment.time_range();

        assert_eq!(range.start, datetime!(2026-09-10 9:00));
        assert_eq!(range.end, datetime!(2026-09-10 9:30));
    }

    #[test]
    fn write_appointment_then_read_appointment_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let vet_id = Uuid::new_v4();
        let appointment = sample_appointment(vet_id, AppointmentStatus::Booked);

        write_appointment(dir.path(), &appointment).unwrap();

        assert_eq!(
            read_appointment(dir.path(), vet_id, appointment.id).unwrap(),
            Some(appointment)
        );
    }

    #[test]
    fn read_active_for_vet_excludes_terminal_statuses() {
        let dir = tempfile::tempdir().unwrap();
        let vet_id = Uuid::new_v4();
        write_appointment(
            dir.path(),
            &sample_appointment(vet_id, AppointmentStatus::Booked),
        )
        .unwrap();
        write_appointment(
            dir.path(),
            &sample_appointment(vet_id, AppointmentStatus::Cancelled),
        )
        .unwrap();

        assert_eq!(read_active_for_vet(dir.path(), vet_id).unwrap().len(), 1);
    }

    #[test]
    fn find_vet_id_for_appointment_locates_the_right_vet() {
        let dir = tempfile::tempdir().unwrap();
        let vet_a = Uuid::new_v4();
        let vet_b = Uuid::new_v4();
        let appointment = sample_appointment(vet_b, AppointmentStatus::Booked);
        write_appointment(
            dir.path(),
            &sample_appointment(vet_a, AppointmentStatus::Booked),
        )
        .unwrap();
        write_appointment(dir.path(), &appointment).unwrap();

        assert_eq!(
            find_vet_id_for_appointment(dir.path(), appointment.id).unwrap(),
            Some(vet_b)
        );
    }

    #[test]
    fn find_vet_id_for_appointment_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(
            find_vet_id_for_appointment(dir.path(), Uuid::new_v4()).unwrap(),
            None
        );
    }

    #[test]
    fn read_all_spans_every_vet() {
        let dir = tempfile::tempdir().unwrap();
        write_appointment(
            dir.path(),
            &sample_appointment(Uuid::new_v4(), AppointmentStatus::Booked),
        )
        .unwrap();
        write_appointment(
            dir.path(),
            &sample_appointment(Uuid::new_v4(), AppointmentStatus::Booked),
        )
        .unwrap();

        assert_eq!(read_all(dir.path()).unwrap().len(), 2);
    }
}
