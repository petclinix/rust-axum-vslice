# Architecture Internals

This document explains the non-obvious design decisions in this application. Each
section describes what a pattern is, why it was chosen, and what breaks if it is not
followed. Where useful, it contrasts the choice with what a database or a framework
— this project deliberately has neither — would normally decide for you instead.

See [`architecture.md`](architecture.md) for the full module-layout/constraint
reference these sections explain the reasoning behind.

---

## 1. Concurrency-Safe Booking: One Exclusive `flock` Per Vet

This is the headline design decision — the whole point of this implementation is to
show what a database's transactions and row locks buy you "for free," reimplemented
by hand over plain files.

### The rule

No double-booking under concurrency, checked as a *time-range overlap*, not
equality — appointment duration varies, so `10:00–11:00` and `10:30–11:30` conflict
even though neither `time_slot` matches the other exactly.

### Why vet granularity, and not finer

Two bookings only conflict if they're for the **same vet** and their time ranges
overlap. `appointments/model.rs::lock_path` returns one lock file per vet,
`locks/vet-<vet_id>.lock`:

```rust
pub fn lock_path(data_dir: &Path, vet_id: Uuid) -> PathBuf {
    data_dir.join("locks").join(format!("vet-{vet_id}.lock"))
}
```

This is exactly as coarse as necessary and no coarser: it serializes every write
for one vet's calendar (book, cancel, reschedule, confirm, complete, no-show) but
never blocks a booking attempt for a *different* vet. It's the direct filesystem
analogue of `php-twig-mtier` taking a `SELECT ... FOR UPDATE` row lock on the vet
being booked before its own overlap check + insert — same shape, a different
substrate.

### The critical section

`appointments::handlers::book_blocking` (trimmed):

```rust
fn book_blocking(
    data_dir: &Path, user_id: Uuid, pet_id: Uuid, vet_id: Uuid,
    time_slot: PrimitiveDateTime, duration: i64,
) -> Result<model::Appointment, AppError> {
    let owner_id = resolve_owner_id(data_dir, user_id)?;
    verify_pet_owned_by(data_dir, pet_id, owner_id)?;

    let _lock = lock::acquire(data_dir, vet_id)?;      // exclusive, blocks until acquired

    let free = free_slots_for(data_dir, vet_id, time_slot.date(), None)?;
    let requested = TimeRange::new(time_slot, time_slot + Duration::minutes(duration));
    if !slots::fits_within_free_slots(&free, &requested) {
        return Err(AppError::SlotUnavailable);
    }

    let appointment = model::Appointment { id: Uuid::new_v4(), pet_id, vet_id, time_slot, duration_minutes: duration, status: AppointmentStatus::Booked };
    model::write_appointment(data_dir, &appointment)?;
    // _lock drops here -> released
    Ok(appointment)
}
```

Everything between acquiring `_lock` and it dropping at the end of the function —
read weekly availability, read that date's exception, read every active
appointment for the vet, derive free slots, check the requested range fits, write
the new record — happens while holding the exclusive lock. Two concurrent booking
attempts for the same vet are fully serialized: the second blocks on `lock::acquire`
until the first's guard drops, then re-derives free slots against the
now-on-disk state the first attempt just wrote. There is no unique-constraint
backstop the way a database gives you for free (§4 below shows why the equivalent
data-modeling discipline matters more here, not less, in its absence) — the lock
*is* the whole correctness mechanism.

Cancel, reschedule, confirm, complete, and no-show all take the **same** vet lock,
even though most of them don't need the overlap check — this is what keeps the
state-machine transitions themselves race-free too (e.g. `confirm` and `cancel`
racing on the same appointment resolve to one winner, not a torn write).

### The read path

`GET /api/vets/{id}/slots` and a vet's own `GET /api/appointments` listing take a
**shared** lock (`storage::FileLock::shared`) around their directory scan — cheap,
allows concurrent readers, and guards against observing this vet's directory
mid-way through a concurrent multi-file write (the single-file atomic rename
already prevents a torn *individual* record, but not a torn *view across* several
files, if one were ever introduced):

```rust
fn get_slots_blocking(data_dir: &Path, vet_id: Uuid, date: Date) -> Result<Vec<TimeRange>, AppError> {
    let _lock = crate::storage::FileLock::shared(&model::lock_path(data_dir, vet_id))?;
    free_slots_for(data_dir, vet_id, date, None)
}
```

---

## 2. Why the Lock Has to Run in `spawn_blocking`, Not the Async Handler Directly

`std::fs::File::lock()` is a blocking OS call — for the exclusive vet lock, it can
genuinely block for as long as another request's entire booking critical section
takes. Calling it directly inside an `async fn` handler would stall whichever tokio
worker thread happens to be running that task: on a multi-threaded runtime with a
handful of worker threads, a burst of concurrent booking attempts for a busy vet
could tie up every worker waiting on `flock`, starving completely unrelated
requests (a health check, a different vet's booking) that would otherwise complete
instantly.

Every handler that does file I/O — not just locking — wraps its blocking work in
`tokio::task::spawn_blocking`, moving it onto tokio's dedicated blocking-thread
pool instead:

```rust
pub async fn book(
    State(config): State<Config>, auth: AuthUser, Json(req): Json<BookRequest>,
) -> Result<(StatusCode, Json<AppointmentResponse>), AppError> {
    // ...cheap, non-blocking validation stays here, before spawning...
    let data_dir = config.data_dir.clone();
    let appointment = tokio::task::spawn_blocking(move || {
        book_blocking(&data_dir, auth.id, pet_id, vet_id, time_slot, duration)
    })
    .await
    .map_err(|_| AppError::Internal)??;   // JoinError, then the inner AppError
    Ok((StatusCode::CREATED, Json(appointment.into())))
}
```

The `??` is deliberate, not a typo: `spawn_blocking` returns
`Result<Result<Appointment, AppError>, JoinError>` — the outer `Result` only ever
fails if the blocking closure itself panicked, mapped to `AppError::Internal`; the
inner one is the closure's real, typed outcome. Validation that doesn't touch the
filesystem (e.g. `duration_minutes` must be positive) stays in the `async fn`
itself, before the `spawn_blocking` call — no reason to pay a thread-pool hop for
work that never blocks.

---

## 3. Reschedule as Cancel-Then-Book Inside One Lock Acquisition

A reschedule is defined as cancel-the-old-appointment-and-book-the-new-one, and it
has to be atomic: if the new slot isn't available, the old appointment must remain
exactly as it was, not left cancelled with nothing to replace it.

`php-twig-mtier` solves the equivalent problem — its `AppointmentRepository::create()`
already opens its own database transaction for the lock-and-overlap-check, and the
reschedule service needs to nest a second logical unit of work inside that — by
making its transaction helper reentrant (its `architecture-internals.md` §2). That
problem doesn't arise here at all, and it's worth saying explicitly why: a
`flock`-based `FileLock` guard is not a transaction with begin/commit semantics that
can be entered twice — it's a plain RAII value. `reschedule_blocking` just does both
steps as ordinary sequential code under one `_lock`:

```rust
fn reschedule_blocking(
    data_dir: &Path, user_id: Uuid, appointment_id: Uuid,
    new_time_slot: PrimitiveDateTime, duration: i64,
) -> Result<model::Appointment, AppError> {
    let owner_id = resolve_owner_id(data_dir, user_id)?;
    let vet_id = model::find_vet_id_for_appointment(data_dir, appointment_id)?
        .ok_or_else(appointment_not_found)?;

    let _lock = lock::acquire(data_dir, vet_id)?;

    let mut old = model::read_appointment(data_dir, vet_id, appointment_id)?
        .ok_or_else(appointment_not_found)?;
    verify_pet_owned_by(data_dir, old.pet_id, owner_id)?;
    if !old.status.can_transition_to(AppointmentStatus::Cancelled) {
        return Err(AppError::InvalidTransition(/* ... */));
    }

    // Exclude the appointment being rescheduled from its own busy set — it
    // currently occupies time that would otherwise block its own new slot.
    let free = free_slots_for(data_dir, vet_id, new_time_slot.date(), Some(appointment_id))?;
    let requested = TimeRange::new(new_time_slot, new_time_slot + Duration::minutes(duration));
    if !slots::fits_within_free_slots(&free, &requested) {
        return Err(AppError::SlotUnavailable);
    }

    old.status = AppointmentStatus::Cancelled;
    model::write_appointment(data_dir, &old)?;

    let new_appointment = model::Appointment { id: Uuid::new_v4(), pet_id: old.pet_id, vet_id, time_slot: new_time_slot, duration_minutes: duration, status: AppointmentStatus::Booked };
    model::write_appointment(data_dir, &new_appointment)?;
    Ok(new_appointment)
}
```

There is no `book_blocking()` call nested inside this function reusing the booking
path's own lock acquisition — that would be the direct equivalent of PHP's nested
transaction problem, calling something that itself tries to acquire the lock again.
Instead the free-slot check is inlined via the same `free_slots_for` helper
`get_slots_blocking` and `book_blocking` both call, parameterized by
`exclude_appointment_id` so the appointment being moved doesn't block its own new
(or identical) slot. One function, one lock, two writes — no reentrancy to design
around because there was never a second lock acquisition to make reentrant.

---

## 4. Proving Concurrency Without Mocking: A Real Multi-Threaded HTTP Stress Test

`features/appointments/tests.rs::concurrent_booking_of_the_same_slot_lets_exactly_one_succeed`
is this repo's proof of correctness for §1 — treated as the acceptance test for the
whole architecture, not just one more unit test. It has to answer a real question:
how do you prove, inside a single `cargo test` process, that many genuinely
concurrent requests for the same vet resolve to exactly one winner?

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_booking_of_the_same_slot_lets_exactly_one_succeed() {
    let fixture = seed();
    const ATTEMPTS: usize = 40;

    let mut handles = Vec::with_capacity(ATTEMPTS);
    for _ in 0..ATTEMPTS {
        let app = fixture.app();
        let payload = book_payload(&fixture, datetime!(2026-09-07 10:00));
        let token = fixture.owner_token.clone();
        handles.push(tokio::spawn(async move {
            call(app, "POST", "/api/appointments", Some(&token), Some(payload)).await
        }));
    }

    let mut results = Vec::with_capacity(ATTEMPTS);
    for handle in handles {
        results.push(handle.await.unwrap());
    }

    let successes = results.iter().filter(|(status, _)| *status == StatusCode::CREATED).count();
    let conflicts = results.iter().filter(|(status, body)| *status == StatusCode::CONFLICT && body["code"] == "SLOT_UNAVAILABLE").count();

    assert_eq!(successes, 1, "exactly one booking attempt should succeed");
    assert_eq!(conflicts, ATTEMPTS - 1);

    let on_disk = model::read_active_for_vet(fixture.data_dir(), fixture.vet_id).unwrap();
    assert_eq!(on_disk.len(), 1, "on-disk appointment count must match the single success");
}
```

Two things make this a genuine test of the real `flock` contention, not of tokio's
own cooperative task scheduling: `flavor = "multi_thread", worker_threads = 8` runs
the test on real OS threads, not a single-threaded executor interleaving tasks
politely at `.await` points; and every attempt goes through `call()`, the same
in-process router the slice's other HTTP tests use — the exact `book_blocking` code
path in §1, not a hand-written approximation of it. The final on-disk assertion
matters as much as the status-code counts: it's possible (with a *wrong*
implementation) for the lock to serialize the HTTP responses correctly while a bug
elsewhere still wrote more than one file — asserting the actual directory contents
closes that gap. This test was run 15 times in a row during development with zero
flakes before being considered done; a flaky result here means the locking is
wrong, not the test.

---

## 5. Recurring Availability: The Exception-Overrides-Template Rule

### The two sources

A vet's bookable hours come from two places: `AvailabilitySlot` (a recurring
weekly template — day-of-week plus a time range) and `AvailabilityException` (a
one-off override for one specific date — either "unavailable" or "custom hours").
Neither stores an actual bookable slot; `appointments::slots::derive_free_slots`
computes them on read.

### An exception fully determines its date

```rust
fn base_windows_for(
    date: Date, weekly: &[AvailabilitySlot], exception: Option<&AvailabilityException>,
) -> Vec<TimeRange> {
    if let Some(exception) = exception {
        return match (exception.is_available, exception.start_time, exception.end_time) {
            (true, Some(start), Some(end)) => vec![TimeRange::new(
                PrimitiveDateTime::new(date, start), PrimitiveDateTime::new(date, end),
            )],
            _ => Vec::new(),   // a day off, or a malformed exception: no bookable time
        };
    }

    let day = DayOfWeek::from(date.weekday());
    weekly.iter().filter(|slot| slot.day_of_week == day)
        .map(|slot| TimeRange::new(PrimitiveDateTime::new(date, slot.start_time), PrimitiveDateTime::new(date, slot.end_time)))
        .collect()
}
```

When an exception exists for the date, the weekly template is not consulted at
all — the `if let Some(exception)` branch returns unconditionally. A vet who
normally works Mondays 9–17 but adds a one-off Monday exception for 14:00–16:00 (a
half-day) must not also see their normal 9–17 hours bleeding through on that date —
that would silently double-offer hours the vet explicitly narrowed. This is a
data-modeling correctness rule, not an implementation detail, and it's exactly the
same shape as `php-twig-mtier`'s identical rule for its own two-table availability
model (its `architecture-internals.md` §4) — this codebase has no database or ORM
to derive it from either, so the same discipline has to be applied by hand here
too.

### Half-open ranges make the boundary cases fall out for free

`domain::TimeRange::overlaps` treats `[start, end)` as half-open:

```rust
pub fn overlaps(&self, other: &TimeRange) -> bool {
    self.start < self.end && other.start < other.end
        && self.start < other.end && other.start < self.end
}
```

Two appointments where one ends exactly when the other starts don't overlap — a
9:00–10:00 appointment immediately followed by a 10:00–11:00 one is a normal back-
to-back schedule, not a conflict. The `self.start < self.end && other.start <
other.end` guard exists because a `[start, end)` range with `start >= end`
represents the empty set, mathematically: without it, a zero-duration instant
sitting inside another range's interior would spuriously "overlap" it — a real bug
caught by this file's own test suite before it ever reached `appointments::slots`,
not a hypothetical.

### The same rule, twice, for now

`locations::slots::derive_free_slots` (§10) ports this exact rule onto
`OpeningPeriod`/`OpeningOverride` instead of `AvailabilitySlot`/
`AvailabilityException` — an override fully determines its date, same as an
exception here. The two implementations coexist because `appointments`' booking
path still reads `availability` directly; `locations` isn't wired into booking
yet. `availability` gets deleted once it is, not before — see §10.

---

## 6. The Pets Wire Contract: Matching the Target Envelope, Not Its Storage

`pets::handlers` matches `docs/petclinix-openapi-snapshot.json`'s `PetRequest`/
`Pet` field-for-field — `species`/`gender` closed enums (`UPPERCASE` on the wire),
camelCase `birthDate`/`pictureContentType`, and inline base64 `picture` (no
`data:` prefix) on both the create/update request and every response:

```rust
#[derive(Debug, Deserialize)]
pub struct PetRequest {
    pub name: String,
    #[serde(default)]
    pub species: Species,                     // defaults to Species::Other
    pub breed: String,
    #[serde(default)]
    pub gender: Gender,                       // defaults to Gender::Unknown
    #[serde(rename = "birthDate")]
    pub birth_date: Date,
    pub picture: String,                      // base64, no `data:` prefix
    #[serde(rename = "pictureContentType")]
    pub picture_content_type: String,
}
```

`species`/`gender` default to their catch-all variant when omitted rather than
being `Option`-wrapped — the target schema only requires `name`, and `Other`/
`Unknown` already mean "not specified," so there's no third "absent" state to
represent. An unrecognized enum value (e.g. `"species": "DRAGON"`) still 422s,
same as any other malformed body — axum's `Json` extractor rejects it before the
handler runs.

Internally, the bytes never live in the small `pets/<id>.json` record — `uploads.rs`
decodes the base64 once, validates it (content-type allowlist, a 5 MiB size cap),
and atomic-writes the raw bytes to `uploads/pets/<id>/picture.<ext>`, storing only
`picture_content_type` in the pet record itself:

```rust
pub struct Pet {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub species: Species,
    pub breed: String,
    pub gender: Gender,
    pub birth_date: Date,
    pub picture_content_type: String,   // no `picture` field here
    pub is_active: bool,                // DELETE /api/pets/{id} flips this
}
```

The response's `id` is `domain::wire_id(pet.id)` — a stable `int64` derived from
the stored `Uuid` (its first 8 bytes, reinterpreted), matching the target
contract's `integer(int64)` id type without this repo giving up `Uuid` as its
storage key: no sequential counter, no extra lock, and existing data stays
addressable across restarts since the mapping only ever depends on the `Uuid`
itself. `GET`/`PUT`/`DELETE /api/pets/{id}` all take that `int64` as the path
param and resolve it back to the owner's `Pet` via
`model::find_by_owner_and_wire_id` — the same directory-scan cost every other
collection read already pays, just scoped by the caller's own pets.

The read path re-encodes those bytes to base64 when building the JSON response.
This is a request/response envelope choice, not a storage choice: on disk it's
still "files instead of a database," just re-serialized as base64 at the API
boundary to match the sibling repo's contract. The raw bytes are written with a
plain `std::fs::write`, not `storage::atomic_write` — that primitive is typed for
`T: Serialize` JSON records, and a torn image write is a visual glitch, not a
correctness invariant the way a torn record would be, so extending `storage`'s
primitive surface just for this one binary case wasn't worth it.

---

## 7. A Wire-Format Bug Caught By a Test, Not By Review: `Display` vs `Serialize`

A test helper building a booking request built its `time_slot` field like this:

```rust
// REJECTED
fn book_payload(fixture: &Fixture, time_slot: PrimitiveDateTime) -> Value {
    json!({ "time_slot": time_slot.to_string(), /* ... */ })
}
```

This passed every test for weeks — every other call site in the test suite hard-
codes a fixed, always-double-digit hour (`datetime!(2026-09-07 10:00)`, `14:00`,
`11:00`). Two tests build `time_slot` from the real current time instead
(`cancel_well_before_the_cutoff_succeeds` / `cancel_after_the_cutoff_is_rejected`,
which have to use "now" to exercise the cancellation-cutoff rule at all), and those
started failing the moment the real clock's UTC hour rolled into single digits:

```
invalid json body: expected value at line 1 column 1
status = 422 Unprocessable Entity
raw = "Failed to deserialize the JSON body into the target type:
       time_slot: the 'hour' component could not be parsed at line 1 column 105"
```

`PrimitiveDateTime`'s `Display` impl (what `.to_string()` calls) does not
zero-pad a single-digit hour — it prints `"9:30:00.0"`, not `"09:30:00.0"`. Every
other part of this codebase constructs the wire format via `Serialize` instead
(what `axum::Json` uses on the server, and what `serde_json::json!` uses for any
non-literal value interpolated into it), which *does* zero-pad — the two code
paths silently disagree on one specific input shape. The fix routes the client
side through the same `Serialize` impl the server already uses, instead of
`Display`:

```rust
fn book_payload(fixture: &Fixture, time_slot: PrimitiveDateTime) -> Value {
    json!({ "time_slot": time_slot, /* ... */ })   // serialized via Serialize, not Display
}
```

The general lesson: when a type has two independent textual representations
(`Display` for humans, `Serialize` for wire formats), reaching for the
more-convenient one at a client call site can silently disagree with what the
server-side `Deserialize` actually accepts — and if the divergence only shows up
for a subset of input values, the bug can pass every test for as long as no test
happens to exercise that subset. It surfaced here specifically because two tests
derive their input from real wall-clock time rather than a fixed literal; a grep
for the same `.to_string()`-on-a-time-value pattern across the rest of the test
suite, once this was found, confirmed it was the only occurrence.

---

## 8. Locating an Appointment by Id Alone: A Bounded Scan, Not a Second Index

Appointments are partitioned by `vet_id` (§1) — but `POST
/api/appointments/{id}/cancel` and `.../reschedule` only carry the appointment id
in the URL. An owner-initiated cancel doesn't know which vet the appointment
belongs to; a vet-initiated one doesn't need to (see below). Resolving "which
vet's directory holds this id" without a secondary index means checking each vet
directory for the matching filename:

```rust
pub fn find_vet_id_for_appointment(data_dir: &Path, appointment_id: Uuid) -> io::Result<Option<Uuid>> {
    let root = appointments_root(data_dir);
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() { continue; }
        let Ok(vet_id) = entry.file_name().to_string_lossy().parse::<Uuid>() else { continue };
        if entry.path().join(format!("{appointment_id}.json")).exists() {
            return Ok(Some(vet_id));
        }
    }
    Ok(None)
}
```

This checks one filename's existence per vet directory — it never parses or reads
an appointment record it isn't looking for, so it isn't the kind of whole-`data/`
scan `architecture.md`'s data-layout section warns against; it's a bounded,
targeted existence check across a directory listing that's cheap regardless of how
many appointments each vet has. `vet_id` never changes for an appointment once
created, so this is safe to do *before* acquiring any lock — by the time the
caller takes `lock::acquire(data_dir, vet_id)`, the vet_id it resolved is still
correct.

**Vet-initiated actions skip this entirely.** `confirm`/`complete`/`no-show`
resolve the calling vet's *own* `vet_id` from their auth token, then read
`appointments/<own_vet_id>/<id>.json` directly — an `O(1)` path lookup, and a vet
attempting to confirm another vet's appointment gets a plain `NotFound` for free,
with no separate ownership check needed, since the file simply isn't in their own
directory.

**The one place a truly unbounded scan is unavoidable**: an owner's `GET
/api/appointments` ("mine") has no single vet to scope to — their pets may have
appointments with several different vets. `appointments::model::read_all` walks
every vet directory and deserializes every record, then the caller filters by
pet ownership. This mirrors the same trade-off `architecture.md` accepts for admin
stats: when the query itself is genuinely global, a global scan is the only way to
answer it correctly.

---

## 9. Testing Strategy: Three Layers, Not One

### Unit / pure-function tests — colocated per module

`storage`, `domain`, and `appointments::slots` in particular get the heaviest
coverage here: no filesystem (`domain`), or a fresh `tempfile::tempdir()` per test
(`storage`, and every slice's `model.rs`). `derive_free_slots` is the one function
in this codebase most worth exhaustively testing in isolation — exact-boundary
slots, an exception overriding (not adding to) the weekly template, zero-duration
appointments — because every other layer's correctness for booking ultimately
depends on it being right, and it has no I/O to fake to test it.

### In-process HTTP tests — colocated per slice (`features/*/tests.rs`)

Each slice's own router, driven via `tower::ServiceExt::oneshot` against a fresh
temp data dir — fast (no real socket, no separate process), and where the bulk of
this codebase's request/response-shape, validation, and authorization coverage
lives. This is also where §4's concurrency stress test lives, since it needs the
real HTTP → handler → lock path, not a bare function call.

### Black-box tests (`tests/` at the crate root) — a genuinely different guarantee

`tests/support/mod.rs::TestServer::spawn` binds a real `TcpListener` on a random
port and serves it with real `axum::serve`, then drives it purely through
`reqwest` over actual HTTP — proving the whole binary's wiring (the real startup
sequence in `main`, including admin seeding; real header/body serialization) works
end to end, not just that one `Router` behaves correctly in memory. `tests/
owner_flow.rs`, `vet_flow.rs`, and `admin_flow.rs` each drive one broad journey
per role — register → discover a vet → book → cancel, book → confirm → complete →
record a visit, seeded admin login → manage users → read stats/activity —
mirroring what a Playwright E2E suite covers functionally in the sibling repos,
lighter-weight since there's no UI to drive. These are deliberately *not* a re-run
of every validation edge case already covered at the slice-test layer above; they
exist to prove the journeys hold together end to end, the same reason
`php-twig-mtier`'s "Portal" tests exist as their own distinct style (its
`architecture-internals.md` §10) — narrower layers don't catch a wiring mistake
that only shows up when several pieces run together for real.

---

## 10. `locations` Replaces Per-Vet `availability` — Eventually

The target contract (`docs/petclinix-openapi-snapshot.json`) has no per-vet
availability endpoints at all — only per-location ones, and its
`BookableLocation` carries a single `vetUsername`, so a location belongs to
exactly one vet. `locations` is the wire-compatible replacement: same
weekly-template-plus-date-override shape as `availability` (§5), just
re-homed under a location a vet explicitly creates (`POST /api/locations`)
instead of a template attached to the vet directly, and a vet can now own
several.

### Not deleting `availability` yet

`appointments`' booking path (`free_slots_for` in `appointments::handlers`)
still reads `availability::model::read_weekly`/`find_exception_by_date`
directly — it hasn't been rewired onto `locations` yet, so deleting
`availability` now would break booking. Both slices' routes are live at once
for now: `/api/vets/availability*` (old) and `/api/locations*` /
`/api/owner/locations*` (new). `availability` gets deleted, and
`appointments::slots` along with it, once booking itself moves onto
`locations::slots::derive_free_slots` in a later pass.

### Wire ids, the same way `pets` does it

`Location.id` on the wire is `domain::wire_id(location.id)`, same derivation
as `pets` (§6) — `Uuid` stays the storage key, `GET`/`PUT`/`DELETE
/api/locations/{id}` resolve the wire id back to it via
`model::find_by_vet_and_wire_id` (scoped to the calling vet — it's their own
data) or `model::find_by_wire_id` (unscoped — an owner discovering a
location to book doesn't know or care which vet it belongs to, the same way
`GET /api/vets/{id}/slots` needs no ownership check today).

### `dayOfWeek` as a wire integer, not an enum string

Unlike this repo's other enums, the target contract's
`OpeningPeriodResponse.dayOfWeek` is a plain `int32`, matching
`java.time.DayOfWeek.getValue()`'s convention (Monday = 1 .. Sunday = 7).
`locations::model::DayOfWeek` stays the same internal enum `availability`
used, serialized `snake_case` for on-disk storage; `handlers.rs` converts at
the boundary via `DayOfWeek::iso_number`/`from_iso_number` rather than
teaching the type itself to serialize two different ways depending on
context.

### Interim approximation: a location's busy time is its vet's busy time

`GET /owner/locations/{id}/available-slots` needs to know what's already
booked at a location. `Appointment` doesn't carry a `location_id` yet (that
lands when `appointments` itself moves onto the target contract), so
`available_slots_blocking` uses the location's vet's *entire* active-
appointment calendar as a stand-in — correct for a vet with exactly one
location, over-blocks one running several, since a booking at location A
would incorrectly also show as busy time at that same vet's location B. This
is a known, temporary approximation, not a modeling decision to keep:
revisit it in the same pass that adds `location_id` to `Appointment`.

### `appointmentType` is accepted, not yet used

The query param is required by the target contract and validated (an
unrecognized value is rejected — via axum's own `Query` extractor
rejection, so a 400, not the `Json` extractor's 422), but nothing in the
domain model ties appointment type to slot duration, and `appointments`
doesn't carry this field yet either. `#[allow(dead_code)]` on the field is
deliberate, not an oversight: the parameter's presence and validation is the
part of the contract being honored right now; folding it into the actual
computation is future work once `appointments` needs the same enum
(`domain::AppointmentType`).
