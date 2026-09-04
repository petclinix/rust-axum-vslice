use std::path::Path;

use axum::Router;
use axum::body::Body;
use axum::http::header::AUTHORIZATION;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use time::OffsetDateTime;
use tower::ServiceExt;
use uuid::Uuid;

use crate::auth::token;
use crate::config::Config;
use crate::domain::{AppointmentStatus, Role};
use crate::features::appointments::model as appointments;
use crate::features::registration::model as registration;

use super::router;

struct SeededUser {
    token: String,
    vet_id: Option<Uuid>,
}

fn seed_user(data_dir: &Path, jwt_secret: &str, role: Role) -> SeededUser {
    let user_id = Uuid::new_v4();

    registration::write_user(
        data_dir,
        &registration::User {
            id: user_id,
            username: format!("{user_id}@example.com"),
            password_hash: "unused".to_string(),
            role,
            is_active: true,
            created_at: OffsetDateTime::now_utc(),
            last_login: None,
        },
    )
    .unwrap();

    let vet_id = if role == Role::Vet {
        let vet = registration::Vet {
            id: Uuid::new_v4(),
            user_id,
            name: "Dr. Bob".to_string(),
            specialty: "Surgery".to_string(),
        };
        registration::write_vet(data_dir, &vet).unwrap();
        Some(vet.id)
    } else {
        None
    };

    let token = token::issue(jwt_secret, &user_id.to_string(), role).unwrap();
    SeededUser { token, vet_id }
}

fn app_for(data_dir: &Path) -> Router {
    router().with_state(Config::for_test(data_dir.to_path_buf()))
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
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, json)
}

fn period(day: i64, start: &str, end: &str) -> Value {
    json!({"dayOfWeek": day, "startTime": start, "endTime": end})
}

fn location_payload(name: &str) -> Value {
    json!({
        "name": name,
        "zoneId": "Europe/Vienna",
        "street": "Main St 1",
        "postalCode": "1010",
        "city": "Vienna",
        "country": "Austria",
        "weeklyPeriods": [period(1, "09:00:00.0", "17:00:00.0")],
        "overrides": [],
    })
}

#[tokio::test]
async fn create_location_without_auth_is_unauthenticated() {
    let dir = tempfile::tempdir().unwrap();

    let (status, _) = call(
        app_for(dir.path()),
        "POST",
        "/api/locations",
        None,
        Some(location_payload("Downtown")),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn owner_role_is_forbidden_from_creating_a_location() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let owner = seed_user(dir.path(), &config.jwt_secret, Role::Owner);

    let (status, _) = call(
        app_for(dir.path()),
        "POST",
        "/api/locations",
        Some(&owner.token),
        Some(location_payload("Downtown")),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn create_location_returns_201_with_periods() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);

    let (status, body) = call(
        app_for(dir.path()),
        "POST",
        "/api/locations",
        Some(&vet.token),
        Some(location_payload("Downtown")),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["name"], "Downtown");
    assert!(body["id"].is_i64());
    let periods = body["weeklyPeriods"].as_array().unwrap();
    assert_eq!(periods.len(), 1);
    assert_eq!(periods[0]["dayOfWeek"], 1);
}

#[tokio::test]
async fn create_location_rejects_start_after_end() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);
    let mut payload = location_payload("Downtown");
    payload["weeklyPeriods"] = json!([period(1, "17:00:00.0", "09:00:00.0")]);

    let (status, body) = call(
        app_for(dir.path()),
        "POST",
        "/api/locations",
        Some(&vet.token),
        Some(payload),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn create_location_rejects_overlapping_periods_on_the_same_day() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);
    let mut payload = location_payload("Downtown");
    payload["weeklyPeriods"] = json!([
        period(1, "09:00:00.0", "12:00:00.0"),
        period(1, "11:00:00.0", "13:00:00.0"),
    ]);

    let (status, _) = call(
        app_for(dir.path()),
        "POST",
        "/api/locations",
        Some(&vet.token),
        Some(payload),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_location_rejects_an_out_of_range_day_of_week() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);
    let mut payload = location_payload("Downtown");
    payload["weeklyPeriods"] = json!([period(8, "09:00:00.0", "12:00:00.0")]);

    let (status, _) = call(
        app_for(dir.path()),
        "POST",
        "/api/locations",
        Some(&vet.token),
        Some(payload),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_location_rejects_an_open_override_missing_times() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);
    let mut payload = location_payload("Downtown");
    payload["overrides"] = json!([{"date": "2026-09-10", "closed": false}]);

    let (status, _) = call(
        app_for(dir.path()),
        "POST",
        "/api/locations",
        Some(&vet.token),
        Some(payload),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_location_rejects_a_closed_override_with_times() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);
    let mut payload = location_payload("Downtown");
    payload["overrides"] = json!([
        {"date": "2026-09-10", "closed": true, "openTime": "09:00:00.0"},
    ]);

    let (status, _) = call(
        app_for(dir.path()),
        "POST",
        "/api/locations",
        Some(&vet.token),
        Some(payload),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_location_rejects_duplicate_override_dates() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);
    let mut payload = location_payload("Downtown");
    payload["overrides"] = json!([
        {"date": "2026-09-10", "closed": true},
        {"date": "2026-09-10", "closed": true},
    ]);

    let (status, _) = call(
        app_for(dir.path()),
        "POST",
        "/api/locations",
        Some(&vet.token),
        Some(payload),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_locations_only_returns_the_callers_own() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet_a = seed_user(dir.path(), &config.jwt_secret, Role::Vet);
    let vet_b = seed_user(dir.path(), &config.jwt_secret, Role::Vet);

    call(
        app_for(dir.path()),
        "POST",
        "/api/locations",
        Some(&vet_a.token),
        Some(location_payload("A's Clinic")),
    )
    .await;
    call(
        app_for(dir.path()),
        "POST",
        "/api/locations",
        Some(&vet_b.token),
        Some(location_payload("B's Clinic")),
    )
    .await;

    let (status, body) = call(
        app_for(dir.path()),
        "GET",
        "/api/locations",
        Some(&vet_a.token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let locations = body.as_array().unwrap();
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0]["name"], "A's Clinic");
}

#[tokio::test]
async fn get_location_owned_by_another_vet_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet_a = seed_user(dir.path(), &config.jwt_secret, Role::Vet);
    let vet_b = seed_user(dir.path(), &config.jwt_secret, Role::Vet);
    let (_, created) = call(
        app_for(dir.path()),
        "POST",
        "/api/locations",
        Some(&vet_a.token),
        Some(location_payload("A's Clinic")),
    )
    .await;
    let location_id = created["id"].as_i64().unwrap();

    let (status, body) = call(
        app_for(dir.path()),
        "GET",
        &format!("/api/locations/{location_id}"),
        Some(&vet_b.token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "NOT_FOUND");
}

#[tokio::test]
async fn update_location_replaces_periods_and_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);
    let (_, created) = call(
        app_for(dir.path()),
        "POST",
        "/api/locations",
        Some(&vet.token),
        Some(location_payload("Downtown")),
    )
    .await;
    let location_id = created["id"].as_i64().unwrap();

    let mut update = location_payload("Uptown");
    update["weeklyPeriods"] = json!([period(5, "08:00:00.0", "10:00:00.0")]);

    let (status, body) = call(
        app_for(dir.path()),
        "PUT",
        &format!("/api/locations/{location_id}"),
        Some(&vet.token),
        Some(update),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "Uptown");
    let periods = body["weeklyPeriods"].as_array().unwrap();
    assert_eq!(periods.len(), 1);
    assert_eq!(periods[0]["dayOfWeek"], 5);
}

#[tokio::test]
async fn delete_location_removes_it() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);
    let (_, created) = call(
        app_for(dir.path()),
        "POST",
        "/api/locations",
        Some(&vet.token),
        Some(location_payload("Downtown")),
    )
    .await;
    let location_id = created["id"].as_i64().unwrap();

    let (status, _) = call(
        app_for(dir.path()),
        "DELETE",
        &format!("/api/locations/{location_id}"),
        Some(&vet.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = call(
        app_for(dir.path()),
        "GET",
        &format!("/api/locations/{location_id}"),
        Some(&vet.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_location_owned_by_another_vet_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet_a = seed_user(dir.path(), &config.jwt_secret, Role::Vet);
    let vet_b = seed_user(dir.path(), &config.jwt_secret, Role::Vet);
    let (_, created) = call(
        app_for(dir.path()),
        "POST",
        "/api/locations",
        Some(&vet_a.token),
        Some(location_payload("A's Clinic")),
    )
    .await;
    let location_id = created["id"].as_i64().unwrap();

    let (status, _) = call(
        app_for(dir.path()),
        "DELETE",
        &format!("/api/locations/{location_id}"),
        Some(&vet_b.token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn owner_can_list_bookable_locations_with_vet_username() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);
    let owner = seed_user(dir.path(), &config.jwt_secret, Role::Owner);
    call(
        app_for(dir.path()),
        "POST",
        "/api/locations",
        Some(&vet.token),
        Some(location_payload("Downtown")),
    )
    .await;

    let (status, body) = call(
        app_for(dir.path()),
        "GET",
        "/api/owner/locations",
        Some(&owner.token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let locations = body.as_array().unwrap();
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0]["name"], "Downtown");
    assert!(locations[0]["vetUsername"].as_str().unwrap().contains('@'));
}

#[tokio::test]
async fn owner_role_is_required_to_list_bookable_locations() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);

    let (status, _) = call(
        app_for(dir.path()),
        "GET",
        "/api/owner/locations",
        Some(&vet.token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn available_slots_reflects_weekly_periods_minus_a_booked_appointment() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);
    let owner = seed_user(dir.path(), &config.jwt_secret, Role::Owner);
    let (_, created) = call(
        app_for(dir.path()),
        "POST",
        "/api/locations",
        Some(&vet.token),
        Some(location_payload("Downtown")),
    )
    .await;
    let location_id = created["id"].as_i64().unwrap();

    // 2026-09-07 is a Monday.
    appointments::write_appointment(
        dir.path(),
        &appointments::Appointment {
            id: Uuid::new_v4(),
            pet_id: Uuid::new_v4(),
            vet_id: vet.vet_id.unwrap(),
            time_slot: time::macros::datetime!(2026-09-07 10:00),
            duration_minutes: 30,
            status: AppointmentStatus::Booked,
        },
    )
    .unwrap();

    let (status, body) = call(
        app_for(dir.path()),
        "GET",
        &format!(
            "/api/owner/locations/{location_id}/available-slots?date=2026-09-07&appointmentType=CHECKUP"
        ),
        Some(&owner.token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let slots = body.as_array().unwrap();
    assert_eq!(slots.len(), 2, "the booking should split the window in two");
    assert_eq!(slots[0]["startsAt"], "2026-09-07 09:00:00.0");
    assert_eq!(slots[0]["endsAt"], "2026-09-07 10:00:00.0");
    assert_eq!(slots[1]["startsAt"], "2026-09-07 10:30:00.0");
    assert_eq!(slots[1]["endsAt"], "2026-09-07 17:00:00.0");
}

#[tokio::test]
async fn available_slots_rejects_an_unknown_appointment_type() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);
    let owner = seed_user(dir.path(), &config.jwt_secret, Role::Owner);
    let (_, created) = call(
        app_for(dir.path()),
        "POST",
        "/api/locations",
        Some(&vet.token),
        Some(location_payload("Downtown")),
    )
    .await;
    let location_id = created["id"].as_i64().unwrap();

    let (status, _) = call(
        app_for(dir.path()),
        "GET",
        &format!(
            "/api/owner/locations/{location_id}/available-slots?date=2026-09-07&appointmentType=DRAGON"
        ),
        Some(&owner.token),
        None,
    )
    .await;

    // Query-string deserialize failures are axum's `Query` extractor's own
    // rejection (400), unlike a malformed JSON body's 422 — a framework
    // distinction, not a choice made here.
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn available_slots_for_a_missing_location_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let owner = seed_user(dir.path(), &config.jwt_secret, Role::Owner);

    let (status, body) = call(
        app_for(dir.path()),
        "GET",
        "/api/owner/locations/999999999/available-slots?date=2026-09-07&appointmentType=CHECKUP",
        Some(&owner.token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "NOT_FOUND");
}
