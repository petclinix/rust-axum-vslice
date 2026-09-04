//! `derive_free_slots` — pure function, no I/O. Free time for
//! one vet on one date = weekly availability (or that date's exception, if
//! any) minus the time already occupied by active appointments.

use time::{Date, PrimitiveDateTime};

use crate::domain::TimeRange;
use crate::features::availability::model::{AvailabilityException, AvailabilitySlot, DayOfWeek};

use super::model::Appointment;

/// The free `[start, end)` windows for `date`, after subtracting active
/// appointments. `exception` must already be resolved to the single
/// exception (if any) for `date` — at most one can exist per the on-disk
/// layout (`docs/architecture.md`).
pub fn derive_free_slots(
    date: Date,
    weekly: &[AvailabilitySlot],
    exception: Option<&AvailabilityException>,
    active_appointments: &[Appointment],
) -> Vec<TimeRange> {
    let base_windows = base_windows_for(date, weekly, exception);

    let busy: Vec<TimeRange> = active_appointments
        .iter()
        .filter(|a| a.is_active() && a.time_slot.date() == date)
        .map(Appointment::time_range)
        .collect();

    base_windows
        .into_iter()
        .flat_map(|window| subtract_busy(window, &busy))
        .collect()
}

/// Whether `requested` fits entirely inside one (not spanning across a gap
/// between two) of `free`'s windows.
pub fn fits_within_free_slots(free: &[TimeRange], requested: &TimeRange) -> bool {
    free.iter()
        .any(|slot| slot.start <= requested.start && requested.end <= slot.end)
}

/// An exception, when present, replaces the weekly recurring schedule for
/// that date entirely — a day off means no windows at all; custom hours
/// means exactly one window, not "weekly windows plus the exception."
fn base_windows_for(
    date: Date,
    weekly: &[AvailabilitySlot],
    exception: Option<&AvailabilityException>,
) -> Vec<TimeRange> {
    if let Some(exception) = exception {
        return match (
            exception.is_available,
            exception.start_time,
            exception.end_time,
        ) {
            (true, Some(start), Some(end)) => {
                vec![TimeRange::new(
                    PrimitiveDateTime::new(date, start),
                    PrimitiveDateTime::new(date, end),
                )]
            }
            // `is_available: false` (a day off), or a malformed exception
            // missing a time despite claiming to be available — both mean
            // no bookable time this date.
            _ => Vec::new(),
        };
    }

    let day = DayOfWeek::from(date.weekday());
    weekly
        .iter()
        .filter(|slot| slot.day_of_week == day)
        .map(|slot| {
            TimeRange::new(
                PrimitiveDateTime::new(date, slot.start_time),
                PrimitiveDateTime::new(date, slot.end_time),
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
/// returned unchanged (this is also what makes an appointment ending
/// exactly when another starts a non-issue — see `TimeRange::overlaps`).
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
    use crate::domain::AppointmentStatus;
    use time::macros::{date, datetime};
    use uuid::Uuid;

    const TODAY: Date = date!(2026 - 09 - 07); // a Monday

    fn weekly_slot(day: DayOfWeek, start: &str, end: &str) -> AvailabilitySlot {
        AvailabilitySlot {
            id: Uuid::new_v4(),
            vet_id: Uuid::new_v4(),
            day_of_week: day,
            start_time: parse_time(start),
            end_time: parse_time(end),
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

    fn booked(time_slot: PrimitiveDateTime, duration_minutes: i64) -> Appointment {
        Appointment {
            id: Uuid::new_v4(),
            pet_id: Uuid::new_v4(),
            vet_id: Uuid::new_v4(),
            time_slot,
            duration_minutes,
            status: AppointmentStatus::Booked,
        }
    }

    #[test]
    fn no_availability_at_all_yields_no_free_slots() {
        let free = derive_free_slots(TODAY, &[], None, &[]);
        assert_eq!(free, Vec::new());
    }

    #[test]
    fn weekly_slot_with_no_appointments_is_fully_free() {
        let weekly = [weekly_slot(DayOfWeek::Monday, "9:00", "12:00")];

        let free = derive_free_slots(TODAY, &weekly, None, &[]);

        assert_eq!(free, vec![range("9:00", "12:00")]);
    }

    #[test]
    fn only_the_matching_day_of_weeks_slots_count() {
        let weekly = [weekly_slot(DayOfWeek::Tuesday, "9:00", "12:00")];

        let free = derive_free_slots(TODAY, &weekly, None, &[]);

        assert_eq!(free, Vec::new());
    }

    #[test]
    fn multiple_weekly_windows_on_the_same_day_are_all_included() {
        let weekly = [
            weekly_slot(DayOfWeek::Monday, "9:00", "12:00"),
            weekly_slot(DayOfWeek::Monday, "13:00", "17:00"),
        ];

        let free = derive_free_slots(TODAY, &weekly, None, &[]);

        assert_eq!(free, vec![range("9:00", "12:00"), range("13:00", "17:00")]);
    }

    #[test]
    fn a_booked_appointment_carves_a_hole_out_of_the_middle() {
        let weekly = [weekly_slot(DayOfWeek::Monday, "9:00", "12:00")];
        let appointments = [booked(datetime!(2026-09-07 10:00), 30)];

        let free = derive_free_slots(TODAY, &weekly, None, &appointments);

        assert_eq!(free, vec![range("9:00", "10:00"), range("10:30", "12:00")]);
    }

    #[test]
    fn appointment_at_the_exact_start_leaves_only_the_tail() {
        let weekly = [weekly_slot(DayOfWeek::Monday, "9:00", "12:00")];
        let appointments = [booked(datetime!(2026-09-07 9:00), 60)];

        let free = derive_free_slots(TODAY, &weekly, None, &appointments);

        assert_eq!(free, vec![range("10:00", "12:00")]);
    }

    #[test]
    fn appointment_touching_the_exact_end_leaves_the_whole_window_free() {
        // Ends exactly when the window ends — half-open, so it doesn't
        // carve anything off ("exact-boundary slots" — see `TimeRange::overlaps`
        // and `docs/architecture-internals.md` §5).
        let weekly = [weekly_slot(DayOfWeek::Monday, "9:00", "12:00")];
        let appointments = [booked(datetime!(2026-09-07 12:00), 30)];

        let free = derive_free_slots(TODAY, &weekly, None, &appointments);

        assert_eq!(free, vec![range("9:00", "12:00")]);
    }

    #[test]
    fn appointment_covering_the_whole_window_leaves_nothing_free() {
        let weekly = [weekly_slot(DayOfWeek::Monday, "9:00", "12:00")];
        let appointments = [booked(datetime!(2026-09-07 9:00), 3 * 60)];

        let free = derive_free_slots(TODAY, &weekly, None, &appointments);

        assert_eq!(free, Vec::new());
    }

    #[test]
    fn appointments_on_a_different_date_do_not_affect_this_date() {
        let weekly = [weekly_slot(DayOfWeek::Monday, "9:00", "12:00")];
        let appointments = [booked(datetime!(2026-08-31 10:00), 30)]; // a different Monday

        let free = derive_free_slots(TODAY, &weekly, None, &appointments);

        assert_eq!(free, vec![range("9:00", "12:00")]);
    }

    #[test]
    fn cancelled_appointments_do_not_block_the_slot() {
        let weekly = [weekly_slot(DayOfWeek::Monday, "9:00", "12:00")];
        let mut appointment = booked(datetime!(2026-09-07 10:00), 30);
        appointment.status = AppointmentStatus::Cancelled;

        let free = derive_free_slots(TODAY, &weekly, None, &[appointment]);

        assert_eq!(free, vec![range("9:00", "12:00")]);
    }

    #[test]
    fn zero_duration_appointment_blocks_nothing() {
        let weekly = [weekly_slot(DayOfWeek::Monday, "9:00", "12:00")];
        let appointments = [booked(datetime!(2026-09-07 10:00), 0)];

        let free = derive_free_slots(TODAY, &weekly, None, &appointments);

        assert_eq!(free, vec![range("9:00", "12:00")]);
    }

    #[test]
    fn exception_day_off_overrides_weekly_availability_entirely() {
        let weekly = [weekly_slot(DayOfWeek::Monday, "9:00", "12:00")];
        let exception = AvailabilityException {
            id: Uuid::new_v4(),
            vet_id: Uuid::new_v4(),
            date: TODAY,
            is_available: false,
            start_time: None,
            end_time: None,
        };

        let free = derive_free_slots(TODAY, &weekly, Some(&exception), &[]);

        assert_eq!(free, Vec::new());
    }

    #[test]
    fn exception_custom_hours_replaces_not_adds_to_the_weekly_window() {
        let weekly = [weekly_slot(DayOfWeek::Monday, "9:00", "12:00")];
        let exception = AvailabilityException {
            id: Uuid::new_v4(),
            vet_id: Uuid::new_v4(),
            date: TODAY,
            is_available: true,
            start_time: Some(parse_time("14:00")),
            end_time: Some(parse_time("16:00")),
        };

        let free = derive_free_slots(TODAY, &weekly, Some(&exception), &[]);

        assert_eq!(free, vec![range("14:00", "16:00")]);
    }

    #[test]
    fn exception_custom_hours_still_subtracts_appointments() {
        let exception = AvailabilityException {
            id: Uuid::new_v4(),
            vet_id: Uuid::new_v4(),
            date: TODAY,
            is_available: true,
            start_time: Some(parse_time("14:00")),
            end_time: Some(parse_time("16:00")),
        };
        let appointments = [booked(datetime!(2026-09-07 15:00), 30)];

        let free = derive_free_slots(TODAY, &[], Some(&exception), &appointments);

        assert_eq!(free, vec![range("14:00", "15:00"), range("15:30", "16:00")]);
    }

    #[test]
    fn fits_within_free_slots_accepts_an_exact_match() {
        let free = vec![range("9:00", "10:00")];
        assert!(fits_within_free_slots(&free, &range("9:00", "10:00")));
    }

    #[test]
    fn fits_within_free_slots_accepts_a_sub_range() {
        let free = vec![range("9:00", "12:00")];
        assert!(fits_within_free_slots(&free, &range("10:00", "10:30")));
    }

    #[test]
    fn fits_within_free_slots_rejects_a_range_spanning_a_gap() {
        let free = vec![range("9:00", "10:00"), range("10:30", "12:00")];
        assert!(!fits_within_free_slots(&free, &range("9:30", "11:00")));
    }

    #[test]
    fn fits_within_free_slots_rejects_when_nothing_is_free() {
        assert!(!fits_within_free_slots(&[], &range("9:00", "9:30")));
    }
}
