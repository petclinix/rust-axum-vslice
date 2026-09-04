//! Visit records and their file I/O. Filename *is* the
//! appointment id — "0..1 per appointment" is enforced by the on-disk
//! layout itself, not by a separate uniqueness check across a directory.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::storage;

/// Matches the target contract's `VetVisitRequest`/`VetVisit` exactly
/// (`docs/petclinix-openapi-snapshot.json`) — three independent free-text
/// fields entered once per appointment, replacing this repo's earlier
/// single typed `{type, remark}` note. `vaccination` blank is a legitimate
/// value (not every visit involves one), not an omission to reject.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Visit {
    pub id: Uuid,
    pub appointment_id: Uuid,
    pub vet_summary: String,
    pub owner_summary: String,
    pub vaccination: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_visit(appointment_id: Uuid) -> Visit {
        Visit {
            id: Uuid::new_v4(),
            appointment_id,
            vet_summary: "Healthy, no concerns.".to_string(),
            owner_summary: "Ate breakfast fine, a bit lethargic.".to_string(),
            vaccination: String::new(),
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
    fn write_visit_overwrites_the_existing_one_for_that_appointment() {
        let dir = tempfile::tempdir().unwrap();
        let appointment_id = Uuid::new_v4();
        let first = sample_visit(appointment_id);
        write_visit(dir.path(), &first).unwrap();

        let updated = Visit {
            vet_summary: "Follow-up: fully recovered.".to_string(),
            ..first
        };
        write_visit(dir.path(), &updated).unwrap();

        assert_eq!(
            find_by_appointment_id(dir.path(), appointment_id).unwrap(),
            Some(updated)
        );
    }
}
