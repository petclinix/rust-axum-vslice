//! Vet-lock acquire helper specific to this slice. Every
//! write path — book/cancel/reschedule/confirm/complete/no-show — goes
//! through `acquire`, even the transitions that don't need the overlap
//! check, so state-machine races (e.g. confirm racing cancel) are excluded
//! too (`docs/architecture-internals.md` §1).

use std::io;
use std::path::Path;
use uuid::Uuid;

use crate::storage::FileLock;

use super::model;

pub fn acquire(data_dir: &Path, vet_id: Uuid) -> io::Result<FileLock> {
    FileLock::exclusive(&model::lock_path(data_dir, vet_id))
}
