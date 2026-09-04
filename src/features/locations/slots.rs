//! `derive_free_slots` — pure function, no I/O. Free time for one location
//! on one date = its weekly opening periods (or that date's override, if
//! any) minus whatever `busy` ranges the caller already resolved (active
//! appointments, from wherever they need to come from — this function
//! doesn't know or care).
//!
//! This is the locations-owned replacement for the old
//! `appointments::slots::derive_free_slots`, which took `&[Appointment]`
//! directly; this one takes plain `&[TimeRange]` instead; so it has no
//! dependency on the `appointments` slice at all, and the caller decides
//! how "busy" gets computed.

use time::{Date, PrimitiveDateTime};

use crate::domain::TimeRange;

use super::model::{DayOfWeek, OpeningOverride, OpeningPeriod};

/// The free `[start, end)` windows for `date`, after subtracting `busy`.
/// `override_` must already be resolved to the single override (if any) for
/// `date` — at most one can exist per location per date, per the on-disk
/// layout (`model::find_override_by_date`).
pub fn derive_free_slots(
    date: Date,
    weekly: &[OpeningPeriod],
    override_: Option<&OpeningOverride>,
    busy: &[TimeRange],
) -> Vec<TimeRange> {
    let base_windows = base_windows_for(date, weekly, override_);

    base_windows
        .into_iter()
        .flat_map(|window| subtract_busy(window, busy))
        .collect()
}

/// An override, when present, replaces the weekly recurring schedule for
/// that date entirely — `closed: true` means no windows at all; custom
/// hours mean exactly one window, not "weekly windows plus the override."
fn base_windows_for(
    date: Date,
    weekly: &[OpeningPeriod],
    override_: Option<&OpeningOverride>,
) -> Vec<TimeRange> {
    if let Some(over) = override_ {
        return match (over.closed, over.open_time, over.close_time) {
            (false, Some(open), Some(close)) => {
                vec![TimeRange::new(
                    PrimitiveDateTime::new(date, open),
                    PrimitiveDateTime::new(date, close),
                )]
            }
            // `closed: true`, or a malformed override missing a time
            // despite claiming to be open — both mean no bookable time
            // this date.
            _ => Vec::new(),
        };
    }

    let day = DayOfWeek::from(date.weekday());
    weekly
        .iter()
        .filter(|period| period.day_of_week == day)
        .map(|period| {
            TimeRange::new(
                PrimitiveDateTime::new(date, period.start_time),
                PrimitiveDateTime::new(date, period.end_time),
            )
        })
        .collect()
}

fn subtract_busy(base: TimeRange, busy: &[TimeRange]) -> Vec<TimeRange> {
    busy.iter().fold(vec![base], |free, busy_range| {
        free.into_iter()
            .flat_map(|window| subtract_one(window, busy_range))
            .collect()
    })
}

/// `a` minus `b`, as 0, 1, or 2 remaining ranges. Relies on `TimeRange`'s
/// half-open semantics: when `a` and `b` don't overlap at all, `a` is
/// returned unchanged.
fn subtract_one(a: TimeRange, b: &TimeRange) -> Vec<TimeRange> {
    if !a.overlaps(b) {
        return vec![a];
    }

    let mut remaining = Vec::new();
    if b.start > a.start {
        remaining.push(TimeRange::new(a.start, b.start));
    }
    if b.end < a.end {
        remaining.push(TimeRange::new(b.end, a.end));
    }
    remaining
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::{date, datetime};
    use uuid::Uuid;

    const TODAY: Date = date!(2026 - 09 - 07); // a Monday

    fn weekly_period(day: DayOfWeek, start: &str, end: &str) -> OpeningPeriod {
        OpeningPeriod {
            id: Uuid::new_v4(),
            location_id: Uuid::new_v4(),
            day_of_week: day,
            start_time: parse_time(start),
            end_time: parse_time(end),
            sort_order: 0,
        }
    }

    fn parse_time(hm: &str) -> time::Time {
        let (h, m) = hm.split_once(':').unwrap();
        time::Time::from_hms(h.parse().unwrap(), m.parse().unwrap(), 0).unwrap()
    }

    fn range(start: &str, end: &str) -> TimeRange {
        TimeRange::new(
            PrimitiveDateTime::new(TODAY, parse_time(start)),
            PrimitiveDateTime::new(TODAY, parse_time(end)),
        )
    }

    fn busy_range(start: PrimitiveDateTime, duration_minutes: i64) -> TimeRange {
        TimeRange::new(start, start + time::Duration::minutes(duration_minutes))
    }

    #[test]
    fn no_availability_at_all_yields_no_free_slots() {
        let free = derive_free_slots(TODAY, &[], None, &[]);
        assert_eq!(free, Vec::new());
    }

    #[test]
    fn weekly_period_with_no_busy_ranges_is_fully_free() {
        let weekly = [weekly_period(DayOfWeek::Monday, "9:00", "12:00")];

        let free = derive_free_slots(TODAY, &weekly, None, &[]);

        assert_eq!(free, vec![range("9:00", "12:00")]);
    }

    #[test]
    fn only_the_matching_day_of_weeks_periods_count() {
        let weekly = [weekly_period(DayOfWeek::Tuesday, "9:00", "12:00")];

        let free = derive_free_slots(TODAY, &weekly, None, &[]);

        assert_eq!(free, Vec::new());
    }

    #[test]
    fn multiple_weekly_windows_on_the_same_day_are_all_included() {
        let weekly = [
            weekly_period(DayOfWeek::Monday, "9:00", "12:00"),
            weekly_period(DayOfWeek::Monday, "13:00", "17:00"),
        ];

        let free = derive_free_slots(TODAY, &weekly, None, &[]);

        assert_eq!(free, vec![range("9:00", "12:00"), range("13:00", "17:00")]);
    }

    #[test]
    fn a_busy_range_carves_a_hole_out_of_the_middle() {
        let weekly = [weekly_period(DayOfWeek::Monday, "9:00", "12:00")];
        let busy = [busy_range(datetime!(2026-09-07 10:00), 30)];

        let free = derive_free_slots(TODAY, &weekly, None, &busy);

        assert_eq!(free, vec![range("9:00", "10:00"), range("10:30", "12:00")]);
    }

    #[test]
    fn busy_range_at_the_exact_start_leaves_only_the_tail() {
        let weekly = [weekly_period(DayOfWeek::Monday, "9:00", "12:00")];
        let busy = [busy_range(datetime!(2026-09-07 9:00), 60)];

        let free = derive_free_slots(TODAY, &weekly, None, &busy);

        assert_eq!(free, vec![range("10:00", "12:00")]);
    }

    #[test]
    fn busy_range_touching_the_exact_end_leaves_the_whole_window_free() {
        let weekly = [weekly_period(DayOfWeek::Monday, "9:00", "12:00")];
        let busy = [busy_range(datetime!(2026-09-07 12:00), 30)];

        let free = derive_free_slots(TODAY, &weekly, None, &busy);

        assert_eq!(free, vec![range("9:00", "12:00")]);
    }

    #[test]
    fn busy_range_covering_the_whole_window_leaves_nothing_free() {
        let weekly = [weekly_period(DayOfWeek::Monday, "9:00", "12:00")];
        let busy = [busy_range(datetime!(2026-09-07 9:00), 3 * 60)];

        let free = derive_free_slots(TODAY, &weekly, None, &busy);

        assert_eq!(free, Vec::new());
    }

    #[test]
    fn zero_duration_busy_range_blocks_nothing() {
        let weekly = [weekly_period(DayOfWeek::Monday, "9:00", "12:00")];
        let busy = [busy_range(datetime!(2026-09-07 10:00), 0)];

        let free = derive_free_slots(TODAY, &weekly, None, &busy);

        assert_eq!(free, vec![range("9:00", "12:00")]);
    }

    #[test]
    fn override_closed_overrides_weekly_availability_entirely() {
        let weekly = [weekly_period(DayOfWeek::Monday, "9:00", "12:00")];
        let over = OpeningOverride {
            id: Uuid::new_v4(),
            location_id: Uuid::new_v4(),
            date: TODAY,
            open_time: None,
            close_time: None,
            closed: true,
            reason: "Holiday".to_string(),
        };

        let free = derive_free_slots(TODAY, &weekly, Some(&over), &[]);

        assert_eq!(free, Vec::new());
    }

    #[test]
    fn override_custom_hours_replaces_not_adds_to_the_weekly_window() {
        let weekly = [weekly_period(DayOfWeek::Monday, "9:00", "12:00")];
        let over = OpeningOverride {
            id: Uuid::new_v4(),
            location_id: Uuid::new_v4(),
            date: TODAY,
            open_time: Some(parse_time("14:00")),
            close_time: Some(parse_time("16:00")),
            closed: false,
            reason: String::new(),
        };

        let free = derive_free_slots(TODAY, &weekly, Some(&over), &[]);

        assert_eq!(free, vec![range("14:00", "16:00")]);
    }

    #[test]
    fn override_custom_hours_still_subtracts_busy_ranges() {
        let over = OpeningOverride {
            id: Uuid::new_v4(),
            location_id: Uuid::new_v4(),
            date: TODAY,
            open_time: Some(parse_time("14:00")),
            close_time: Some(parse_time("16:00")),
            closed: false,
            reason: String::new(),
        };
        let busy = [busy_range(datetime!(2026-09-07 15:00), 30)];

        let free = derive_free_slots(TODAY, &[], Some(&over), &busy);

        assert_eq!(free, vec![range("14:00", "15:00"), range("15:30", "16:00")]);
    }
}
