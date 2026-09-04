//! A vet's clinic location — address + a recurring weekly opening
//! schedule + one-off date overrides — and their file I/O. Partitioned by
//! `location_id`, the same way `appointments` partitions by `vet_id`: a
//! read or write for one location never touches another's files. Replaces
//! this repo's earlier per-vet `availability` slice — the target contract
//! (`docs/petclinix-openapi-snapshot.json`) has no per-vet availability
//! endpoints at all, only per-location ones, and its `BookableLocation`
//! carries a single `vetUsername`, so a location belongs to exactly one vet.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::{Date, Time};
use uuid::Uuid;

use crate::domain;
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

impl DayOfWeek {
    /// The target contract's `OpeningPeriodResponse.dayOfWeek` is a plain
    /// `int32`, matching `java.time.DayOfWeek.getValue()`'s convention:
    /// Monday = 1 .. Sunday = 7.
    pub fn iso_number(self) -> i32 {
        match self {
            DayOfWeek::Monday => 1,
            DayOfWeek::Tuesday => 2,
            DayOfWeek::Wednesday => 3,
            DayOfWeek::Thursday => 4,
            DayOfWeek::Friday => 5,
            DayOfWeek::Saturday => 6,
            DayOfWeek::Sunday => 7,
        }
    }

    pub fn from_iso_number(n: i32) -> Option<Self> {
        match n {
            1 => Some(DayOfWeek::Monday),
            2 => Some(DayOfWeek::Tuesday),
            3 => Some(DayOfWeek::Wednesday),
            4 => Some(DayOfWeek::Thursday),
            5 => Some(DayOfWeek::Friday),
            6 => Some(DayOfWeek::Saturday),
            7 => Some(DayOfWeek::Sunday),
            _ => None,
        }
    }
}

impl From<time::Weekday> for DayOfWeek {
    /// Cross-slice: `derive_free_slots` needs to turn a calendar `Date`
    /// into the `DayOfWeek` its weekly schedule is keyed by.
    fn from(weekday: time::Weekday) -> Self {
        match weekday {
            time::Weekday::Monday => DayOfWeek::Monday,
            time::Weekday::Tuesday => DayOfWeek::Tuesday,
            time::Weekday::Wednesday => DayOfWeek::Wednesday,
            time::Weekday::Thursday => DayOfWeek::Thursday,
            time::Weekday::Friday => DayOfWeek::Friday,
            time::Weekday::Saturday => DayOfWeek::Saturday,
            time::Weekday::Sunday => DayOfWeek::Sunday,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Location {
    pub id: Uuid,
    pub vet_id: Uuid,
    pub name: String,
    pub zone_id: String,
    pub street: String,
    pub postal_code: String,
    pub city: String,
    pub country: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpeningPeriod {
    pub id: Uuid,
    pub location_id: Uuid,
    pub day_of_week: DayOfWeek,
    pub start_time: Time,
    pub end_time: Time,
    pub sort_order: i32,
}

/// A one-off override for a specific date — `closed: true` (no times) or
/// custom hours (`closed: false`, both times set), replacing the recurring
/// weekly schedule for that one day only. At most one per location per
/// date, same invariant `availability`'s exceptions had.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpeningOverride {
    pub id: Uuid,
    pub location_id: Uuid,
    pub date: Date,
    pub open_time: Option<Time>,
    pub close_time: Option<Time>,
    pub closed: bool,
    pub reason: String,
}

fn locations_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("locations")
}

fn location_path(data_dir: &Path, id: Uuid) -> PathBuf {
    locations_dir(data_dir).join(format!("{id}.json"))
}

fn periods_dir(data_dir: &Path, location_id: Uuid) -> PathBuf {
    data_dir
        .join("opening_periods")
        .join(location_id.to_string())
}

fn period_path(data_dir: &Path, location_id: Uuid, id: Uuid) -> PathBuf {
    periods_dir(data_dir, location_id).join(format!("{id}.json"))
}

fn overrides_dir(data_dir: &Path, location_id: Uuid) -> PathBuf {
    data_dir
        .join("opening_overrides")
        .join(location_id.to_string())
}

fn override_path(data_dir: &Path, location_id: Uuid, id: Uuid) -> PathBuf {
    overrides_dir(data_dir, location_id).join(format!("{id}.json"))
}

/// A vet editing one of their own locations (address fields, weekly
/// periods, or overrides) — same rationale as the registration slice's
/// `users.lock` or the old `availability-<vet_id>.lock`, just keyed by
/// location instead of vet, since a vet can now own several.
pub fn lock_path(data_dir: &Path, location_id: Uuid) -> PathBuf {
    data_dir
        .join("locks")
        .join(format!("location-{location_id}.lock"))
}

pub fn write_location(data_dir: &Path, location: &Location) -> io::Result<()> {
    storage::atomic_write(&location_path(data_dir, location.id), location)
}

pub fn read_location(data_dir: &Path, id: Uuid) -> io::Result<Option<Location>> {
    storage::read_json(&location_path(data_dir, id))
}

pub fn list_all_locations(data_dir: &Path) -> io::Result<Vec<Location>> {
    storage::list_dir_json(&locations_dir(data_dir))
}

pub fn list_locations_for_vet(data_dir: &Path, vet_id: Uuid) -> io::Result<Vec<Location>> {
    let locations = list_all_locations(data_dir)?;
    Ok(locations
        .into_iter()
        .filter(|l| l.vet_id == vet_id)
        .collect())
}

/// Resolves a wire id (`domain::wire_id`) back to the `Location` it was
/// derived from, scoped to `vet_id` — same directory-scan trade-off as
/// `pets::model::find_by_owner_and_wire_id`.
pub fn find_by_vet_and_wire_id(
    data_dir: &Path,
    vet_id: Uuid,
    wire_id: i64,
) -> io::Result<Option<Location>> {
    let locations = list_locations_for_vet(data_dir, vet_id)?;
    Ok(locations
        .into_iter()
        .find(|l| domain::wire_id(l.id) == wire_id))
}

/// Unscoped lookup by wire id — owners discovering a location to book
/// don't know (or care) which vet it belongs to, the same way today's
/// `GET /api/vets/{id}/slots` needs no ownership check: it's public
/// discovery data, not something the caller must already own.
pub fn find_by_wire_id(data_dir: &Path, wire_id: i64) -> io::Result<Option<Location>> {
    let locations = list_all_locations(data_dir)?;
    Ok(locations
        .into_iter()
        .find(|l| domain::wire_id(l.id) == wire_id))
}

/// Removes the location record and every period/override file under it —
/// the target contract has no `active` flag on `Location` (unlike `Pet` or
/// `AdminUserResponse`), so this is a hard delete, not a soft one.
pub fn delete_location(data_dir: &Path, id: Uuid) -> io::Result<()> {
    match std::fs::remove_file(location_path(data_dir, id)) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    remove_dir_all_if_present(&periods_dir(data_dir, id))?;
    remove_dir_all_if_present(&overrides_dir(data_dir, id))?;
    Ok(())
}

fn remove_dir_all_if_present(dir: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

pub fn write_period(data_dir: &Path, period: &OpeningPeriod) -> io::Result<()> {
    storage::atomic_write(
        &period_path(data_dir, period.location_id, period.id),
        period,
    )
}

pub fn read_periods(data_dir: &Path, location_id: Uuid) -> io::Result<Vec<OpeningPeriod>> {
    storage::list_dir_json(&periods_dir(data_dir, location_id))
}

/// Full replace of a location's weekly schedule, same idiom as the old
/// `availability::delete_all_slots` — called while holding the location's
/// exclusive lock, immediately before writing the new set, so a concurrent
/// reader never observes the directory between "old periods gone" and "new
/// periods written."
pub fn delete_all_periods(data_dir: &Path, location_id: Uuid) -> io::Result<()> {
    remove_dir_all_if_present(&periods_dir(data_dir, location_id))
}

pub fn write_override(data_dir: &Path, over: &OpeningOverride) -> io::Result<()> {
    storage::atomic_write(&override_path(data_dir, over.location_id, over.id), over)
}

pub fn read_overrides(data_dir: &Path, location_id: Uuid) -> io::Result<Vec<OpeningOverride>> {
    storage::list_dir_json(&overrides_dir(data_dir, location_id))
}

pub fn delete_all_overrides(data_dir: &Path, location_id: Uuid) -> io::Result<()> {
    remove_dir_all_if_present(&overrides_dir(data_dir, location_id))
}

pub fn find_override_by_date(
    data_dir: &Path,
    location_id: Uuid,
    date: Date,
) -> io::Result<Option<OpeningOverride>> {
    let overrides = read_overrides(data_dir, location_id)?;
    Ok(overrides.into_iter().find(|o| o.date == date))
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::{date, time};

    #[test]
    fn day_of_week_iso_number_round_trips() {
        for day in [
            DayOfWeek::Monday,
            DayOfWeek::Tuesday,
            DayOfWeek::Wednesday,
            DayOfWeek::Thursday,
            DayOfWeek::Friday,
            DayOfWeek::Saturday,
            DayOfWeek::Sunday,
        ] {
            assert_eq!(DayOfWeek::from_iso_number(day.iso_number()), Some(day));
        }
        assert_eq!(DayOfWeek::from_iso_number(0), None);
        assert_eq!(DayOfWeek::from_iso_number(8), None);
    }

    #[test]
    fn day_of_week_from_time_weekday_matches_by_name() {
        assert_eq!(DayOfWeek::from(time::Weekday::Monday), DayOfWeek::Monday);
        assert_eq!(DayOfWeek::from(time::Weekday::Sunday), DayOfWeek::Sunday);
        assert_eq!(
            DayOfWeek::from(date!(2026 - 09 - 07).weekday()),
            DayOfWeek::Monday
        );
    }

    fn sample_location(vet_id: Uuid) -> Location {
        Location {
            id: Uuid::new_v4(),
            vet_id,
            name: "Downtown Clinic".to_string(),
            zone_id: "Europe/Vienna".to_string(),
            street: "Main St 1".to_string(),
            postal_code: "1010".to_string(),
            city: "Vienna".to_string(),
            country: "Austria".to_string(),
        }
    }

    #[test]
    fn write_location_then_read_location_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let location = sample_location(Uuid::new_v4());

        write_location(dir.path(), &location).unwrap();

        assert_eq!(
            read_location(dir.path(), location.id).unwrap(),
            Some(location)
        );
    }

    #[test]
    fn list_locations_for_vet_only_returns_that_vets_locations() {
        let dir = tempfile::tempdir().unwrap();
        let vet_a = Uuid::new_v4();
        let vet_b = Uuid::new_v4();
        write_location(dir.path(), &sample_location(vet_a)).unwrap();
        write_location(dir.path(), &sample_location(vet_a)).unwrap();
        write_location(dir.path(), &sample_location(vet_b)).unwrap();

        assert_eq!(list_locations_for_vet(dir.path(), vet_a).unwrap().len(), 2);
    }

    #[test]
    fn find_by_vet_and_wire_id_matches_and_no_match_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let vet_id = Uuid::new_v4();
        let location = sample_location(vet_id);
        write_location(dir.path(), &location).unwrap();

        assert_eq!(
            find_by_vet_and_wire_id(dir.path(), vet_id, domain::wire_id(location.id)).unwrap(),
            Some(location)
        );
        assert_eq!(
            find_by_vet_and_wire_id(dir.path(), vet_id, 123456).unwrap(),
            None
        );
    }

    #[test]
    fn find_by_wire_id_is_unscoped_by_vet() {
        let dir = tempfile::tempdir().unwrap();
        let location = sample_location(Uuid::new_v4());
        write_location(dir.path(), &location).unwrap();

        assert_eq!(
            find_by_wire_id(dir.path(), domain::wire_id(location.id)).unwrap(),
            Some(location)
        );
    }

    #[test]
    fn delete_location_removes_the_record_and_its_periods_and_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let location = sample_location(Uuid::new_v4());
        write_location(dir.path(), &location).unwrap();
        write_period(
            dir.path(),
            &OpeningPeriod {
                id: Uuid::new_v4(),
                location_id: location.id,
                day_of_week: DayOfWeek::Monday,
                start_time: time!(9:00),
                end_time: time!(12:00),
                sort_order: 0,
            },
        )
        .unwrap();
        write_override(
            dir.path(),
            &OpeningOverride {
                id: Uuid::new_v4(),
                location_id: location.id,
                date: date!(2026 - 09 - 10),
                open_time: None,
                close_time: None,
                closed: true,
                reason: "Holiday".to_string(),
            },
        )
        .unwrap();

        delete_location(dir.path(), location.id).unwrap();

        assert_eq!(read_location(dir.path(), location.id).unwrap(), None);
        assert_eq!(read_periods(dir.path(), location.id).unwrap(), Vec::new());
        assert_eq!(read_overrides(dir.path(), location.id).unwrap(), Vec::new());
    }

    #[test]
    fn delete_location_that_does_not_exist_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        delete_location(dir.path(), Uuid::new_v4()).unwrap();
    }

    #[test]
    fn write_period_then_read_periods_returns_it() {
        let dir = tempfile::tempdir().unwrap();
        let location_id = Uuid::new_v4();
        let period = OpeningPeriod {
            id: Uuid::new_v4(),
            location_id,
            day_of_week: DayOfWeek::Monday,
            start_time: time!(9:00),
            end_time: time!(12:00),
            sort_order: 0,
        };

        write_period(dir.path(), &period).unwrap();

        assert_eq!(read_periods(dir.path(), location_id).unwrap(), vec![period]);
    }

    #[test]
    fn delete_all_periods_clears_a_locations_weekly_schedule() {
        let dir = tempfile::tempdir().unwrap();
        let location_id = Uuid::new_v4();
        write_period(
            dir.path(),
            &OpeningPeriod {
                id: Uuid::new_v4(),
                location_id,
                day_of_week: DayOfWeek::Monday,
                start_time: time!(9:00),
                end_time: time!(12:00),
                sort_order: 0,
            },
        )
        .unwrap();

        delete_all_periods(dir.path(), location_id).unwrap();

        assert_eq!(read_periods(dir.path(), location_id).unwrap(), Vec::new());
    }

    #[test]
    fn write_override_then_find_by_date_matches() {
        let dir = tempfile::tempdir().unwrap();
        let location_id = Uuid::new_v4();
        let over = OpeningOverride {
            id: Uuid::new_v4(),
            location_id,
            date: date!(2026 - 09 - 03),
            open_time: None,
            close_time: None,
            closed: true,
            reason: "Holiday".to_string(),
        };
        write_override(dir.path(), &over).unwrap();

        assert_eq!(
            find_override_by_date(dir.path(), location_id, date!(2026 - 09 - 03)).unwrap(),
            Some(over)
        );
        assert_eq!(
            find_override_by_date(dir.path(), location_id, date!(2026 - 09 - 04)).unwrap(),
            None
        );
    }
}
