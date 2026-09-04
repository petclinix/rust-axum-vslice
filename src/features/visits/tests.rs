use std::path::Path;

use axum::Router;
use axum::body::Body;
use axum::http::header::AUTHORIZATION;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::{date, datetime};
use tower::ServiceExt;
use uuid::Uuid;

use crate::auth::token;
use crate::config::Config;
use crate::domain::{self, AppointmentStatus, AppointmentType, Role};
use crate::features::appointments::model as appointments;
use crate::features::pets::model as pets;
use crate::features::registration::model as registration;

use super::router;

struct Fixture {
    dir: tempfile::TempDir,
    config: Config,
    owner_token: String,
    vet_token: String,
    pet_id: Uuid,
    vet_id: Uuid,
}

impl Fixture {
    fn data_dir(&self) -> &Path {
        self.dir.path()
    }

    fn app(&self) -> Router {
        router().with_state(self.config.clone())
    }
}

/// Seeds an owner + their pet and a vet — no location, since these tests
/// seed appointments directly at whatever status they need rather than
/// booking through the HTTP API.
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
    let owner_token = token::issue(
        &config.jwt_secret,
        &owner_user_id.to_string(),
        &format!("{owner_user_id}@example.com"),
        Role::Owner,
    )
    .unwrap();

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
    let vet_token = token::issue(
        &config.jwt_secret,
        &vet_user_id.to_string(),
        &format!("{vet_user_id}@example.com"),
        Role::Vet,
    )
    .unwrap();

    Fixture {
        dir,
        config,
        owner_token,
        vet_token,
        pet_id,
        vet_id,
    }
}

fn seed_appointment(fixture: &Fixture, status: AppointmentStatus) -> Uuid {
    let appointment = appointments::Appointment {
        id: Uuid::new_v4(),
        pet_id: fixture.pet_id,
        vet_id: fixture.vet_id,
        location_id: Uuid::new_v4(),
        time_slot: datetime!(2026-09-07 10:00),
        duration_minutes: 30,
        status,
        appointment_type: AppointmentType::Checkup,
    };
    appointments::write_appointment(fixture.data_dir(), &appointment).unwrap();
    appointment.id
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

fn visit_payload() -> Value {
    json!({
        "vetSummary": "Healthy, no concerns.",
        "ownerSummary": "Ate breakfast fine, a bit lethargic.",
        "vaccination": "",
    })
}

fn vet_visit_url(appointment_id: Uuid) -> String {
    format!("/api/vet/visits/{}", domain::wire_id(appointment_id))
}

#[tokio::test]
async fn put_vet_visit_creates_it_and_completes_a_confirmed_appointment() {
    let fixture = seed();
    let appointment_id = seed_appointment(&fixture, AppointmentStatus::Confirmed);

    let (status, body) = call(
        fixture.app(),
        "PUT",
        &vet_visit_url(appointment_id),
        Some(&fixture.vet_token),
        Some(visit_payload()),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["vetSummary"], "Healthy, no concerns.");
    assert_eq!(body["ownerSummary"], "Ate breakfast fine, a bit lethargic.");
    assert_eq!(body["vaccination"], "");
    assert!(body["id"].is_i64());

    // This file only mounts `visits::router()`, not the full app, so the
    // resulting status is checked by reading the appointment record
    // directly rather than through `GET /api/vet/appointments`.
    let appointment =
        appointments::read_appointment(fixture.data_dir(), fixture.vet_id, appointment_id)
            .unwrap()
            .unwrap();
    assert_eq!(appointment.status, AppointmentStatus::Completed);
}

#[tokio::test]
async fn get_vet_visit_after_put_returns_it() {
    let fixture = seed();
    let appointment_id = seed_appointment(&fixture, AppointmentStatus::Confirmed);
    call(
        fixture.app(),
        "PUT",
        &vet_visit_url(appointment_id),
        Some(&fixture.vet_token),
        Some(visit_payload()),
    )
    .await;

    let (status, body) = call(
        fixture.app(),
        "GET",
        &vet_visit_url(appointment_id),
        Some(&fixture.vet_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["vetSummary"], "Healthy, no concerns.");
}

#[tokio::test]
async fn get_vet_visit_for_an_appointment_with_no_visit_is_not_found() {
    let fixture = seed();
    let appointment_id = seed_appointment(&fixture, AppointmentStatus::Confirmed);

    let (status, body) = call(
        fixture.app(),
        "GET",
        &vet_visit_url(appointment_id),
        Some(&fixture.vet_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "NOT_FOUND");
}

#[tokio::test]
async fn put_vet_visit_requires_the_vet_role() {
    let fixture = seed();
    let appointment_id = seed_appointment(&fixture, AppointmentStatus::Confirmed);

    let (status, _) = call(
        fixture.app(),
        "PUT",
        &vet_visit_url(appointment_id),
        Some(&fixture.owner_token),
        Some(visit_payload()),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn put_vet_visit_on_a_merely_booked_appointment_is_an_invalid_transition() {
    let fixture = seed();
    let appointment_id = seed_appointment(&fixture, AppointmentStatus::Booked);

    let (status, body) = call(
        fixture.app(),
        "PUT",
        &vet_visit_url(appointment_id),
        Some(&fixture.vet_token),
        Some(visit_payload()),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "INVALID_TRANSITION");
}

#[tokio::test]
async fn put_vet_visit_twice_updates_it_in_place() {
    let fixture = seed();
    let appointment_id = seed_appointment(&fixture, AppointmentStatus::Confirmed);
    let (_, first) = call(
        fixture.app(),
        "PUT",
        &vet_visit_url(appointment_id),
        Some(&fixture.vet_token),
        Some(visit_payload()),
    )
    .await;

    let mut updated_payload = visit_payload();
    updated_payload["vetSummary"] = json!("Follow-up: fully recovered.");
    let (status, second) = call(
        fixture.app(),
        "PUT",
        &vet_visit_url(appointment_id),
        Some(&fixture.vet_token),
        Some(updated_payload),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["id"], first["id"], "same visit, updated in place");
    assert_eq!(second["vetSummary"], "Follow-up: fully recovered.");
}

#[tokio::test]
async fn put_vet_visit_by_a_non_owning_vet_is_not_found() {
    let fixture = seed();
    let appointment_id = seed_appointment(&fixture, AppointmentStatus::Confirmed);

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
        &format!("{other_vet_user_id}@example.com"),
        Role::Vet,
    )
    .unwrap();

    let (status, _) = call(
        fixture.app(),
        "PUT",
        &vet_visit_url(appointment_id),
        Some(&other_vet_token),
        Some(visit_payload()),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_owner_pet_visits_returns_only_appointments_with_a_recorded_visit() {
    let fixture = seed();
    let visited = seed_appointment(&fixture, AppointmentStatus::Confirmed);
    seed_appointment(&fixture, AppointmentStatus::Confirmed); // no visit recorded
    call(
        fixture.app(),
        "PUT",
        &vet_visit_url(visited),
        Some(&fixture.vet_token),
        Some(visit_payload()),
    )
    .await;

    let (status, body) = call(
        fixture.app(),
        "GET",
        &format!("/api/owner/pets/{}/visits", domain::wire_id(fixture.pet_id)),
        Some(&fixture.owner_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let visits = body.as_array().unwrap();
    assert_eq!(visits.len(), 1);
    assert_eq!(
        visits[0]["ownerSummary"],
        "Ate breakfast fine, a bit lethargic."
    );
    assert!(visits[0]["vetUsername"].as_str().unwrap().contains('@'));
    assert_eq!(visits[0]["startsAt"], "2026-09-07 10:00:00.0");
    assert!(visits[0].get("vetSummary").is_none());
}

#[tokio::test]
async fn list_owner_pet_visits_requires_the_owner_role() {
    let fixture = seed();

    let (status, _) = call(
        fixture.app(),
        "GET",
        &format!("/api/owner/pets/{}/visits", domain::wire_id(fixture.pet_id)),
        Some(&fixture.vet_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn list_owner_pet_visits_for_someone_elses_pet_is_not_found() {
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
    registration::write_owner(
        fixture.data_dir(),
        &registration::Owner {
            id: Uuid::new_v4(),
            user_id: other_owner_user_id,
            name: "Mallory".to_string(),
            phone: "555-0199".to_string(),
        },
    )
    .unwrap();
    let other_token = token::issue(
        &fixture.config.jwt_secret,
        &other_owner_user_id.to_string(),
        &format!("{other_owner_user_id}@example.com"),
        Role::Owner,
    )
    .unwrap();

    let (status, _) = call(
        fixture.app(),
        "GET",
        &format!("/api/owner/pets/{}/visits", domain::wire_id(fixture.pet_id)),
        Some(&other_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}
