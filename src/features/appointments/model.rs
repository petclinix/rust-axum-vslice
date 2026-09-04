//! Appointment records and their file I/O. Partitioned by
//! `vet_id`, exactly like the old `availability` slice was — a booking
//! attempt only ever locks and scans one vet's directory
//! (`docs/architecture-internals.md` §1). Each appointment also carries the
//! `location_id` it was booked at (the target contract's
//! `Appointment.locationId`), but conflict-checking and the exclusive lock
//! both stay vet-scoped, not location-scoped — a vet has one calendar
//! across every location they run, not a separate one per location (see
//! `locations::slots`'s doc comment).

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::{Duration, PrimitiveDateTime};
use uuid::Uuid;

use crate::domain::{self, AppointmentStatus, AppointmentType, TimeRange};
use crate::storage;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Appointment {
    pub id: Uuid,
    pub pet_id: Uuid,
    pub vet_id: Uuid,
    pub location_id: Uuid,
    pub time_slot: PrimitiveDateTime,
    pub duration_minutes: i64,
    pub status: AppointmentStatus,
    pub appointment_type: AppointmentType,
}

impl Appointment {
    /// `Booked`/`Confirmed` occupy time on the calendar and block new
    /// bookings; `Completed`/`Cancelled`/`NoShow` don't
    /// (`docs/architecture-internals.md` §1).
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

/// The headline lock (`docs/architecture-internals.md` §1): every write path (book, cancel,
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

/// Every appointment across every vet — used by the owner-scoped "my
/// appointments" listing (an owner's pets may have appointments with
/// several vets) and by wire-id resolution, both of which have no single
/// vet to scope to. Same rationale as admin stats
/// (`docs/architecture.md`'s data-layout section): a global scan is
/// unavoidable when the query itself is global, not vet-scoped.
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

/// Resolves a wire id (`domain::wire_id`) back to the `Appointment` it was
/// derived from — a global scan, same trade-off as `read_all`, since the
/// on-disk layout has no secondary index from wire id (or even real id) to
/// vet id. Used by both the owner- and vet-facing routes to locate an
/// appointment from a path param before taking the vet's lock and
/// re-reading it fresh under that lock (avoids trusting a pre-lock read for
/// the write itself).
pub fn find_by_wire_id(data_dir: &Path, wire_id: i64) -> io::Result<Option<Appointment>> {
    Ok(read_all(data_dir)?
        .into_iter()
        .find(|a| domain::wire_id(a.id) == wire_id))
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
            location_id: Uuid::new_v4(),
            time_slot: datetime!(2026-09-10 9:00),
            duration_minutes: 30,
            status,
            appointment_type: AppointmentType::Checkup,
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

    #[test]
    fn find_by_wire_id_matches_and_no_match_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let appointment = sample_appointment(Uuid::new_v4(), AppointmentStatus::Booked);
        write_appointment(dir.path(), &appointment).unwrap();

        assert_eq!(
            find_by_wire_id(dir.path(), domain::wire_id(appointment.id)).unwrap(),
            Some(appointment)
        );
        assert_eq!(find_by_wire_id(dir.path(), 123456).unwrap(), None);
    }
}
