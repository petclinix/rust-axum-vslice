use std::path::Path;

use axum::Router;
use axum::body::Body;
use axum::http::header::AUTHORIZATION;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use time::macros::{date, datetime, time};
use time::{Duration, OffsetDateTime};
use tower::ServiceExt;
use uuid::Uuid;

use crate::auth::token;
use crate::config::Config;
use crate::domain::{self, Role};
use crate::features::locations::model::{self as locations, DayOfWeek, Location, OpeningPeriod};
use crate::features::pets::model as pets;
use crate::features::registration::model as registration;

use super::model;
use super::router;

struct Fixture {
    dir: tempfile::TempDir,
    config: Config,
    owner_token: String,
    pet_id: i64,
    vet_token: String,
    vet_id: Uuid,
    location_id: i64,
}

impl Fixture {
    fn data_dir(&self) -> &Path {
        self.dir.path()
    }

    fn app(&self) -> Router {
        router().with_state(self.config.clone())
    }
}

/// Seeds an owner + their pet, a vet, and a location with a Monday
/// 9:00-17:00 weekly opening period for that vet. Using a fixed weekday
/// (rather than "today") keeps every non-cutoff test deterministic
/// regardless of when it runs.
fn seed() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let data_dir = dir.path();

    let owner_user_id = Uuid::new_v4();
    registration::write_user(
        data_dir,
        &registration::User {
            id: owner_user_id,
            username: format!("{owner_user_id}@example.com"),
            password_hash: "unused".to_string(),
            role: Role::Owner,
            is_active: true,
            created_at: OffsetDateTime::now_utc(),
            last_login: None,
        },
    )
    .unwrap();
    let owner_id = Uuid::new_v4();
    registration::write_owner(
        data_dir,
        &registration::Owner {
            id: owner_id,
            user_id: owner_user_id,
            name: "Alice".to_string(),
            phone: "555-0100".to_string(),
        },
    )
    .unwrap();
    let owner_token =
        token::issue(&config.jwt_secret, &owner_user_id.to_string(), Role::Owner).unwrap();

    let pet_id = Uuid::new_v4();
    pets::write_pet(
        data_dir,
        &pets::Pet {
            id: pet_id,
            owner_id,
            name: "Rex".to_string(),
            species: pets::Species::Dog,
            breed: "Labrador".to_string(),
            gender: pets::Gender::Male,
            birth_date: date!(2020 - 01 - 01),
            picture_content_type: "image/jpeg".to_string(),
            is_active: true,
        },
    )
    .unwrap();

    let vet_user_id = Uuid::new_v4();
    registration::write_user(
        data_dir,
        &registration::User {
            id: vet_user_id,
            username: format!("{vet_user_id}@example.com"),
            password_hash: "unused".to_string(),
            role: Role::Vet,
            is_active: true,
            created_at: OffsetDateTime::now_utc(),
            last_login: None,
        },
    )
    .unwrap();
    let vet_id = Uuid::new_v4();
    registration::write_vet(
        data_dir,
        &registration::Vet {
            id: vet_id,
            user_id: vet_user_id,
            name: "Dr. Bob".to_string(),
            specialty: "Surgery".to_string(),
        },
    )
    .unwrap();
    let vet_token = token::issue(&config.jwt_secret, &vet_user_id.to_string(), Role::Vet).unwrap();

    let location_id = Uuid::new_v4();
    locations::write_location(
        data_dir,
        &Location {
            id: location_id,
            vet_id,
            name: "Downtown Clinic".to_string(),
            zone_id: "Europe/Vienna".to_string(),
            street: "Main St 1".to_string(),
            postal_code: "1010".to_string(),
            city: "Vienna".to_string(),
            country: "Austria".to_string(),
        },
    )
    .unwrap();
    locations::write_period(
        data_dir,
        &OpeningPeriod {
            id: Uuid::new_v4(),
            location_id,
            day_of_week: DayOfWeek::Monday,
            start_time: time!(9:00),
            end_time: time!(17:00),
            sort_order: 0,
        },
    )
    .unwrap();

    Fixture {
        dir,
        config,
        owner_token,
        pet_id: domain::wire_id(pet_id),
        vet_token,
        vet_id,
        location_id: domain::wire_id(location_id),
    }
}

async fn call(
    app: Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        builder = builder.header(AUTHORIZATION, format!("Bearer {t}"));
    }
    let body = match body {
        Some(v) => {
            builder = builder.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, json)
}

fn book_payload(fixture: &Fixture, starts_at: time::PrimitiveDateTime) -> Value {
    // Serialize via serde (`json!` calls `Serialize`, not `Display`) so this
    // produces exactly the zero-padded wire format the server's own
    // `Deserialize` impl expects. `PrimitiveDateTime`'s `Display` (i.e.
    // `.to_string()`) does *not* zero-pad a single-digit hour the way its
    // `Serialize` impl does — using it here intermittently broke this
    // payload once a day, for whichever hour happened to be single-digit.
    json!({
        "locationId": fixture.location_id,
        "petId": fixture.pet_id,
        "startsAt": starts_at,
        "appointmentType": "CHECKUP",
    })
}

#[tokio::test]
async fn book_without_auth_is_unauthenticated() {
    let fixture = seed();

    let (status, _) = call(
        fixture.app(),
        "POST",
        "/api/owner/appointments",
        None,
        Some(book_payload(&fixture, datetime!(2026-09-07 10:00))),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn book_success_returns_201_booked() {
    let fixture = seed();

    let (status, body) = call(
        fixture.app(),
        "POST",
        "/api/owner/appointments",
        Some(&fixture.owner_token),
        Some(book_payload(&fixture, datetime!(2026-09-07 10:00))),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["status"], "BOOKED");
    assert_eq!(body["petId"], fixture.pet_id);
    assert_eq!(body["locationId"], fixture.location_id);
}

#[tokio::test]
async fn book_outside_availability_is_slot_unavailable() {
    let fixture = seed();

    // Tuesday: no weekly period seeded for it.
    let (status, body) = call(
        fixture.app(),
        "POST",
        "/api/owner/appointments",
        Some(&fixture.owner_token),
        Some(book_payload(&fixture, datetime!(2026-09-08 10:00))),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "SLOT_UNAVAILABLE");
}

#[tokio::test]
async fn book_conflicting_with_an_existing_appointment_is_rejected() {
    let fixture = seed();
    call(
        fixture.app(),
        "POST",
        "/api/owner/appointments",
        Some(&fixture.owner_token),
        Some(book_payload(&fixture, datetime!(2026-09-07 10:00))),
    )
    .await;

    let (status, body) = call(
        fixture.app(),
        "POST",
        "/api/owner/appointments",
        Some(&fixture.owner_token),
        Some(book_payload(&fixture, datetime!(2026-09-07 10:15))),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "SLOT_UNAVAILABLE");
}

#[tokio::test]
async fn book_with_someone_elses_pet_is_not_found() {
    let fixture = seed();
    let other_owner_user_id = Uuid::new_v4();
    registration::write_user(
        fixture.data_dir(),
        &registration::User {
            id: other_owner_user_id,
            username: format!("{other_owner_user_id}@example.com"),
            password_hash: "unused".to_string(),
            role: Role::Owner,
            is_active: true,
            created_at: OffsetDateTime::now_utc(),
            last_login: None,
        },
    )
    .unwrap();
    let other_owner_id = Uuid::new_v4();
    registration::write_owner(
        fixture.data_dir(),
        &registration::Owner {
            id: other_owner_id,
            user_id: other_owner_user_id,
            name: "Mallory".to_string(),
            phone: "555-0199".to_string(),
        },
    )
    .unwrap();
    let other_token = token::issue(
        &fixture.config.jwt_secret,
        &other_owner_user_id.to_string(),
        Role::Owner,
    )
    .unwrap();

    let (status, _) = call(
        fixture.app(),
        "POST",
        "/api/owner/appointments",
        Some(&other_token),
        Some(book_payload(&fixture, datetime!(2026-09-07 10:00))),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

async fn book(fixture: &Fixture, starts_at: time::PrimitiveDateTime) -> i64 {
    let (status, body) = call(
        fixture.app(),
        "POST",
        "/api/owner/appointments",
        Some(&fixture.owner_token),
        Some(book_payload(fixture, starts_at)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "booking setup failed: {body}");
    body["id"].as_i64().unwrap()
}

#[tokio::test]
async fn cancel_well_before_the_cutoff_succeeds() {
    let fixture = seed();
    seed_full_week_availability(fixture.data_dir(), fixture.vet_id);
    let starts_at = snap_to_hour(OffsetDateTime::now_utc() + Duration::hours(48));
    let id = book(&fixture, starts_at).await;

    let (status, _) = call(
        fixture.app(),
        "DELETE",
        &format!("/api/owner/appointments/{id}"),
        Some(&fixture.owner_token),
        None,
    )
    .await;

    // The target contract declares this endpoint `200 OK` with no content —
    // unlike `POST`/`GET`/reschedule, which return the `Appointment` body.
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn cancel_after_the_cutoff_is_rejected() {
    let fixture = seed();
    seed_full_week_availability(fixture.data_dir(), fixture.vet_id);
    let starts_at = snap_to_hour(OffsetDateTime::now_utc() + Duration::hours(1));
    let id = book(&fixture, starts_at).await;

    let (status, body) = call(
        fixture.app(),
        "DELETE",
        &format!("/api/owner/appointments/{id}"),
        Some(&fixture.owner_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "CANCELLATION_CUTOFF_PASSED");
}

#[tokio::test]
async fn confirm_then_complete_flow() {
    let fixture = seed();
    let id = book(&fixture, datetime!(2026-09-07 10:00)).await;

    let (status, _) = call(
        fixture.app(),
        "PUT",
        &format!("/api/vet/appointments/{id}/confirm"),
        Some(&fixture.vet_token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(status_of(&fixture, id).await, "CONFIRMED");

    let (status, body) = call(
        fixture.app(),
        "POST",
        &format!("/api/appointments/{id}/complete"),
        Some(&fixture.vet_token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "COMPLETED");
}

/// `PUT .../confirm` and `.../no-show` return no body (see
/// `cancel_well_before_the_cutoff_succeeds`'s comment), so tests that need
/// to observe the resulting status look it up via the vet's calendar
/// instead.
async fn status_of(fixture: &Fixture, id: i64) -> String {
    let (_, body) = call(
        fixture.app(),
        "GET",
        "/api/vet/appointments",
        Some(&fixture.vet_token),
        None,
    )
    .await;
    body.as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == id)
        .expect("booked appointment should be on the vet's calendar")["status"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn no_show_on_a_merely_booked_appointment_is_an_invalid_transition() {
    let fixture = seed();
    let id = book(&fixture, datetime!(2026-09-07 10:00)).await;

    let (status, body) = call(
        fixture.app(),
        "PUT",
        &format!("/api/vet/appointments/{id}/no-show"),
        Some(&fixture.vet_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "INVALID_TRANSITION");
}

#[tokio::test]
async fn a_vet_cannot_confirm_another_vets_appointment() {
    let fixture = seed();
    let id = book(&fixture, datetime!(2026-09-07 10:00)).await;

    let other_vet_user_id = Uuid::new_v4();
    registration::write_user(
        fixture.data_dir(),
        &registration::User {
            id: other_vet_user_id,
            username: format!("{other_vet_user_id}@example.com"),
            password_hash: "unused".to_string(),
            role: Role::Vet,
            is_active: true,
            created_at: OffsetDateTime::now_utc(),
            last_login: None,
        },
    )
    .unwrap();
    registration::write_vet(
        fixture.data_dir(),
        &registration::Vet {
            id: Uuid::new_v4(),
            user_id: other_vet_user_id,
            name: "Dr. Eve".to_string(),
            specialty: "Dentistry".to_string(),
        },
    )
    .unwrap();
    let other_vet_token = token::issue(
        &fixture.config.jwt_secret,
        &other_vet_user_id.to_string(),
        Role::Vet,
    )
    .unwrap();

    let (status, _) = call(
        fixture.app(),
        "PUT",
        &format!("/api/vet/appointments/{id}/confirm"),
        Some(&other_vet_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn reschedule_moves_the_appointment_and_frees_the_old_slot() {
    let fixture = seed();
    let id = book(&fixture, datetime!(2026-09-07 10:00)).await;

    let (status, body) = call(
        fixture.app(),
        "PUT",
        &format!("/api/owner/appointments/{id}/reschedule"),
        Some(&fixture.owner_token),
        Some(json!({"startsAt": "2026-09-07 14:00:00.0"})),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "BOOKED");
    assert_eq!(body["startsAt"], "2026-09-07 14:00:00.0");
    assert_ne!(body["id"], id);

    // The old slot is free again — a fresh booking there should succeed.
    let (status, _) = call(
        fixture.app(),
        "POST",
        "/api/owner/appointments",
        Some(&fixture.owner_token),
        Some(book_payload(&fixture, datetime!(2026-09-07 10:00))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn reschedule_into_an_occupied_slot_is_rejected() {
    let fixture = seed();
    let id = book(&fixture, datetime!(2026-09-07 10:00)).await;
    book(&fixture, datetime!(2026-09-07 14:00)).await;

    let (status, body) = call(
        fixture.app(),
        "PUT",
        &format!("/api/owner/appointments/{id}/reschedule"),
        Some(&fixture.owner_token),
        Some(json!({"startsAt": "2026-09-07 14:00:00.0"})),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "SLOT_UNAVAILABLE");
}

#[tokio::test]
async fn list_mine_as_owner_returns_only_their_pets_appointments() {
    let fixture = seed();
    book(&fixture, datetime!(2026-09-07 10:00)).await;

    let (status, body) = call(
        fixture.app(),
        "GET",
        "/api/owner/appointments",
        Some(&fixture.owner_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn list_mine_as_vet_returns_their_calendar() {
    let fixture = seed();
    book(&fixture, datetime!(2026-09-07 10:00)).await;

    let (status, body) = call(
        fixture.app(),
        "GET",
        "/api/vet/appointments",
        Some(&fixture.vet_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let appointments = body.as_array().unwrap();
    assert_eq!(appointments.len(), 1);
    assert_eq!(appointments[0]["petName"], "Rex");
    assert!(
        appointments[0]["ownerUsername"]
            .as_str()
            .unwrap()
            .contains('@')
    );
}

/// The concurrency stress test (`docs/architecture-internals.md` §§1/4) — this repo's proof of
/// correctness. Many concurrent booking attempts for the same overlapping
/// slot, for the same vet, against one shared data dir: exactly one must
/// succeed and the rest must see `SlotUnavailable`, and the on-disk active
/// appointment count for the vet must match. Runs on a genuinely
/// multi-threaded runtime so the `flock` contention is real OS-thread
/// contention, not single-thread cooperative scheduling.
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
            let (status, body) = call(
                app,
                "POST",
                "/api/owner/appointments",
                Some(&token),
                Some(payload),
            )
            .await;
            (status, body)
        }));
    }

    let mut results = Vec::with_capacity(ATTEMPTS);
    for handle in handles {
        results.push(handle.await.unwrap());
    }

    let successes = results
        .iter()
        .filter(|(status, _)| *status == StatusCode::CREATED)
        .count();
    let conflicts = results
        .iter()
        .filter(|(status, body)| {
            *status == StatusCode::CONFLICT && body["code"] == "SLOT_UNAVAILABLE"
        })
        .count();

    assert_eq!(successes, 1, "exactly one booking attempt should succeed");
    assert_eq!(conflicts, ATTEMPTS - 1);

    let on_disk = model::read_active_for_vet(fixture.data_dir(), fixture.vet_id).unwrap();
    assert_eq!(
        on_disk.len(),
        1,
        "on-disk appointment count must match the single success"
    );
}

fn seed_full_week_availability(data_dir: &Path, vet_id: Uuid) {
    let location = locations::list_locations_for_vet(data_dir, vet_id)
        .unwrap()
        .into_iter()
        .next()
        .expect("seed() already wrote one location for this vet");
    locations::delete_all_periods(data_dir, location.id).unwrap();

    for day in [
        DayOfWeek::Monday,
        DayOfWeek::Tuesday,
        DayOfWeek::Wednesday,
        DayOfWeek::Thursday,
        DayOfWeek::Friday,
        DayOfWeek::Saturday,
        DayOfWeek::Sunday,
    ] {
        locations::write_period(
            data_dir,
            &OpeningPeriod {
                id: Uuid::new_v4(),
                location_id: location.id,
                day_of_week: day,
                start_time: time::Time::MIDNIGHT,
                end_time: time!(23:59:59),
                sort_order: 0,
            },
        )
        .unwrap();
    }
}

/// Snaps to the start of the current hour. Combined with the full-week
/// availability window above running to 23:59:59, even the latest possible
/// snapped start (23:00) plus a 30-minute booking comfortably fits before
/// midnight — no cross-midnight edge case to worry about here.
fn snap_to_hour(when: OffsetDateTime) -> time::PrimitiveDateTime {
    time::PrimitiveDateTime::new(
        when.date(),
        time::Time::from_hms(when.hour(), 0, 0).unwrap(),
    )
}
