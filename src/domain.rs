//! Types shared by more than one vertical slice (see `docs/architecture.md`,
//! "Module Layout"). Nothing here
//! does file I/O — these are pure types and pure functions only; anything
//! that needs `storage` belongs in a slice's own `model.rs`, not here.

use serde::{Deserialize, Serialize};
use time::PrimitiveDateTime;
use uuid::Uuid;

/// A user's role in the system (see `docs/architecture.md`, "Auth Design").
/// `Owner` and `Vet` self-register;
/// `Admin` is seeded on first boot and never created through the register
/// endpoint. Wire-cased `UPPERCASE` to match the target contract in
/// `docs/petclinix-openapi-snapshot.json` (`ADMIN`/`VET`/`OWNER`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Role {
    Owner,
    Vet,
    Admin,
}

/// A stable `int64` derived from a `Uuid`, for the wire only. This repo
/// keeps `Uuid` as the on-disk storage key everywhere — it's the internal
/// identity, and path params still resolve back to it — but the target
/// wire contract (`docs/petclinix-openapi-snapshot.json`) types every `id`
/// as `integer(int64)`, so responses expose this derived value instead of
/// the UUID itself. Taking the first 8 bytes rather than hashing keeps this
/// a pure reinterpretation (no collision-handling logic needed beyond what
/// a UUID already gives you), and deterministic: the same UUID always maps
/// to the same wire id, which is what a client polling by id needs.
pub fn wire_id(id: Uuid) -> i64 {
    let bytes = id.as_bytes();
    i64::from_be_bytes(bytes[0..8].try_into().expect("uuid is 16 bytes"))
}

/// The appointment lifecycle (see `docs/architecture.md`'s API Surface
/// section): `Booked → Confirmed →
/// Completed/Cancelled/NoShow`. `Cancelled` is reachable from both `Booked`
/// and `Confirmed` (cancellation is cutoff-gated, not confirm-gated).
/// `Completed`, `Cancelled`, and `NoShow` are terminal — nothing transitions
/// out of them. Wire-cased `SCREAMING_SNAKE_CASE` to match the target
/// contract in `docs/petclinix-openapi-snapshot.json`
/// (`BOOKED`/`CONFIRMED`/`COMPLETED`/`CANCELLED`/`NO_SHOW`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AppointmentStatus {
    Booked,
    Confirmed,
    Completed,
    Cancelled,
    NoShow,
}

/// The kind of appointment being booked — a closed enum on the wire
/// (`docs/petclinix-openapi-snapshot.json`'s `AppointmentRequest`/
/// `Appointment`/`VetAppointment`). Lives here rather than in
/// `appointments` or `locations` because both need it: `locations`' free-
/// slot query takes it as a filter param, `appointments` will carry it on
/// the booked record itself once that slice moves to the target contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AppointmentType {
    Vaccination,
    FollowUp,
    Checkup,
    Emergency,
    Surgery,
}

impl AppointmentStatus {
    /// Whether the state machine allows moving from `self` to `next`. A
    /// handler must reject any transition this returns `false` for with a
    /// typed `AppError::InvalidTransition`, not a generic 400.
    pub fn can_transition_to(self, next: AppointmentStatus) -> bool {
        use AppointmentStatus::*;

        matches!(
            (self, next),
            (Booked, Confirmed)
                | (Booked, Cancelled)
                | (Confirmed, Completed)
                | (Confirmed, Cancelled)
                | (Confirmed, NoShow)
        )
    }
}

/// A half-open `[start, end)` time interval — the shared representation for
/// an appointment slot, an availability window, and everything
/// `derive_free_slots` computes over (`docs/architecture-internals.md` §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub start: PrimitiveDateTime,
    pub end: PrimitiveDateTime,
}

impl TimeRange {
    pub fn new(start: PrimitiveDateTime, end: PrimitiveDateTime) -> Self {
        Self { start, end }
    }

    /// Whether `self` and `other` share any point in time. Half-open at
    /// both ends, so two ranges that only touch at a boundary (one's `end`
    /// equals the other's `start`) do **not** overlap — booking the 10:00
    /// slot right after a 9:00–10:00 appointment is allowed, not a
    /// double-booking. A `[start, end)` range with `start >= end` contains
    /// no instants at all, so — same as an empty set intersected with
    /// anything — it never overlaps, even a range it sits inside of.
    pub fn overlaps(&self, other: &TimeRange) -> bool {
        self.start < self.end
            && other.start < other.end
            && self.start < other.end
            && other.start < self.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    mod wire_id {
        use super::*;

        #[test]
        fn is_stable_for_the_same_uuid() {
            let id = Uuid::new_v4();
            assert_eq!(wire_id(id), wire_id(id));
        }

        #[test]
        fn differs_across_a_batch_of_random_uuids() {
            let ids: Vec<i64> = (0..1000).map(|_| wire_id(Uuid::new_v4())).collect();
            let mut unique = ids.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(
                unique.len(),
                ids.len(),
                "expected no collisions in 1000 random uuids"
            );
        }
    }

    mod appointment_status_transitions {
        use super::*;
        use AppointmentStatus::*;

        #[test]
        fn booked_can_be_confirmed_or_cancelled() {
            assert!(Booked.can_transition_to(Confirmed));
            assert!(Booked.can_transition_to(Cancelled));
        }

        #[test]
        fn booked_cannot_be_completed_or_marked_no_show_directly() {
            assert!(!Booked.can_transition_to(Completed));
            assert!(!Booked.can_transition_to(NoShow));
        }

        #[test]
        fn confirmed_can_be_completed_cancelled_or_marked_no_show() {
            assert!(Confirmed.can_transition_to(Completed));
            assert!(Confirmed.can_transition_to(Cancelled));
            assert!(Confirmed.can_transition_to(NoShow));
        }

        #[test]
        fn confirmed_cannot_be_re_booked() {
            assert!(!Confirmed.can_transition_to(Booked));
        }

        #[test]
        fn terminal_states_allow_no_further_transitions() {
            let all = [Booked, Confirmed, Completed, Cancelled, NoShow];

            for terminal in [Completed, Cancelled, NoShow] {
                for next in all {
                    assert!(
                        !terminal.can_transition_to(next),
                        "{terminal:?} -> {next:?} should not be allowed"
                    );
                }
            }
        }

        #[test]
        fn no_status_transitions_to_itself() {
            for status in [Booked, Confirmed, Completed, Cancelled, NoShow] {
                assert!(!status.can_transition_to(status));
            }
        }
    }

    mod time_range_overlap {
        use super::*;

        fn range(start: &str, end: &str) -> TimeRange {
            // Small helper so each test reads as plain "9:00-10:00" instead
            // of repeating the full 2026-09-03 date on every literal.
            fn parse(hm: &str) -> PrimitiveDateTime {
                let (h, m) = hm.split_once(':').unwrap();
                let time = time::Time::from_hms(h.parse().unwrap(), m.parse().unwrap(), 0).unwrap();
                PrimitiveDateTime::new(datetime!(2026-09-03 0:00).date(), time)
            }
            TimeRange::new(parse(start), parse(end))
        }

        #[test]
        fn identical_ranges_overlap() {
            assert!(range("9:00", "10:00").overlaps(&range("9:00", "10:00")));
        }

        #[test]
        fn partially_overlapping_ranges_overlap() {
            let a = range("9:00", "10:00");
            let b = range("9:30", "10:30");

            assert!(a.overlaps(&b));
            assert!(b.overlaps(&a));
        }

        #[test]
        fn one_range_fully_containing_another_overlaps() {
            assert!(range("9:00", "12:00").overlaps(&range("10:00", "11:00")));
            assert!(range("10:00", "11:00").overlaps(&range("9:00", "12:00")));
        }

        #[test]
        fn disjoint_ranges_do_not_overlap() {
            assert!(!range("9:00", "10:00").overlaps(&range("11:00", "12:00")));
            assert!(!range("11:00", "12:00").overlaps(&range("9:00", "10:00")));
        }

        #[test]
        fn ranges_touching_exactly_at_a_boundary_do_not_overlap() {
            // 9-10 immediately followed by 10-11: allowed, not a
            // double-booking.
            assert!(!range("9:00", "10:00").overlaps(&range("10:00", "11:00")));
            assert!(!range("10:00", "11:00").overlaps(&range("9:00", "10:00")));
        }

        #[test]
        fn zero_duration_range_never_overlaps_anything() {
            let instant = range("9:00", "9:00");

            assert!(!instant.overlaps(&range("8:00", "10:00")));
            assert!(!instant.overlaps(&instant));
        }
    }
}
