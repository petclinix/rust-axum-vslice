//! Visit records and their file I/O (PLAN.md §4). Filename *is* the
//! appointment id — "0..1 per appointment" is enforced by the on-disk
//! layout itself, not by a separate uniqueness check across a directory.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::storage;

/// What a vet records at the end of a completed appointment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisitType {
    Diagnosis,
    Vaccination,
    Note,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Visit {
    pub id: Uuid,
    pub appointment_id: Uuid,
    #[serde(rename = "type")]
    pub visit_type: VisitType,
    pub remark: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

fn visits_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("visits")
}

fn visit_path(data_dir: &Path, appointment_id: Uuid) -> PathBuf {
    visits_dir(data_dir).join(format!("{appointment_id}.json"))
}

pub fn write_visit(data_dir: &Path, visit: &Visit) -> io::Result<()> {
    storage::atomic_write(&visit_path(data_dir, visit.appointment_id), visit)
}

pub fn find_by_appointment_id(data_dir: &Path, appointment_id: Uuid) -> io::Result<Option<Visit>> {
    storage::read_json(&visit_path(data_dir, appointment_id))
}

/// Cross-slice helper: given a set of appointment ids (typically "every
/// appointment for one pet"), returns whichever of them have a recorded
/// visit. Used by both this slice's own `GET /api/pets/{id}/visits` and the
/// `pets` slice's `GET /api/pets/{id}` detail view (PLAN.md §9).
pub fn find_all_for_appointments(
    data_dir: &Path,
    appointment_ids: &[Uuid],
) -> io::Result<Vec<Visit>> {
    let mut visits = Vec::new();
    for &id in appointment_ids {
        if let Some(visit) = find_by_appointment_id(data_dir, id)? {
            visits.push(visit);
        }
    }
    Ok(visits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_visit(appointment_id: Uuid) -> Visit {
        Visit {
            id: Uuid::new_v4(),
            appointment_id,
            visit_type: VisitType::Diagnosis,
            remark: "Healthy, no concerns.".to_string(),
            created_at: OffsetDateTime::now_utc(),
        }
    }

    #[test]
    fn write_visit_then_find_by_appointment_id_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let appointment_id = Uuid::new_v4();
        let visit = sample_visit(appointment_id);

        write_visit(dir.path(), &visit).unwrap();

        assert_eq!(
            find_by_appointment_id(dir.path(), appointment_id).unwrap(),
            Some(visit)
        );
    }

    #[test]
    fn find_by_appointment_id_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(
            find_by_appointment_id(dir.path(), Uuid::new_v4()).unwrap(),
            None
        );
    }

    #[test]
    fn find_all_for_appointments_skips_appointments_with_no_visit() {
        let dir = tempfile::tempdir().unwrap();
        let with_visit = Uuid::new_v4();
        let without_visit = Uuid::new_v4();
        let visit = sample_visit(with_visit);
        write_visit(dir.path(), &visit).unwrap();

        let found = find_all_for_appointments(dir.path(), &[with_visit, without_visit]).unwrap();

        assert_eq!(found, vec![visit]);
    }
}
