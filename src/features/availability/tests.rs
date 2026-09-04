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
use crate::domain::Role;
use crate::features::registration::model as registration;

use super::model;
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

async fn call(app: Router, uri: &str, token: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("POST").uri(uri);
    if let Some(t) = token {
        builder = builder.header(AUTHORIZATION, format!("Bearer {t}"));
    }
    builder = builder.header("content-type", "application/json");
    let response = app
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            panic!(
                "invalid json body: {e}; raw = {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, json)
}

// `time::Time`'s default JSON representation always carries a fractional-
// seconds component (e.g. "09:00:00.0"), even at whole-second precision —
// confirmed by probing `serde_json::to_string` on a `time!(9:00)` literal.
fn slot(day: &str, start: &str, end: &str) -> Value {
    json!({"day_of_week": day, "start_time": start, "end_time": end})
}

#[tokio::test]
async fn set_weekly_without_auth_is_unauthenticated() {
    let dir = tempfile::tempdir().unwrap();

    let (status, _) = call(
        app_for(dir.path()),
        "/api/vets/availability",
        None,
        json!({"slots": [slot("monday", "09:00:00.0", "12:00:00.0")]}),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn owner_role_is_forbidden_from_availability_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let owner = seed_user(dir.path(), &config.jwt_secret, Role::Owner);

    let (status, _) = call(
        app_for(dir.path()),
        "/api/vets/availability",
        Some(&owner.token),
        json!({"slots": [slot("monday", "09:00:00.0", "12:00:00.0")]}),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn set_weekly_returns_the_new_slots() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);

    let (status, body) = call(
        app_for(dir.path()),
        "/api/vets/availability",
        Some(&vet.token),
        json!({"slots": [
            slot("monday", "09:00:00.0", "12:00:00.0"),
            slot("wednesday", "13:00:00.0", "17:00:00.0"),
        ]}),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let slots = body.as_array().unwrap();
    assert_eq!(slots.len(), 2);
    assert_eq!(
        model::read_weekly(dir.path(), vet.vet_id.unwrap())
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn set_weekly_replaces_the_previous_schedule() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);

    call(
        app_for(dir.path()),
        "/api/vets/availability",
        Some(&vet.token),
        json!({"slots": [slot("monday", "09:00:00.0", "12:00:00.0")]}),
    )
    .await;
    call(
        app_for(dir.path()),
        "/api/vets/availability",
        Some(&vet.token),
        json!({"slots": [slot("friday", "08:00:00.0", "10:00:00.0")]}),
    )
    .await;

    let remaining = model::read_weekly(dir.path(), vet.vet_id.unwrap()).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].day_of_week, model::DayOfWeek::Friday);
}

#[tokio::test]
async fn set_weekly_rejects_start_after_end() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);

    let (status, body) = call(
        app_for(dir.path()),
        "/api/vets/availability",
        Some(&vet.token),
        json!({"slots": [slot("monday", "12:00:00.0", "09:00:00.0")]}),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn set_weekly_rejects_overlapping_slots_on_the_same_day() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);

    let (status, _) = call(
        app_for(dir.path()),
        "/api/vets/availability",
        Some(&vet.token),
        json!({"slots": [
            slot("monday", "09:00:00.0", "12:00:00.0"),
            slot("monday", "11:00:00.0", "13:00:00.0"),
        ]}),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn set_weekly_allows_touching_slots_on_the_same_day() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);

    let (status, _) = call(
        app_for(dir.path()),
        "/api/vets/availability",
        Some(&vet.token),
        json!({"slots": [
            slot("monday", "09:00:00.0", "12:00:00.0"),
            slot("monday", "12:00:00.0", "13:00:00.0"),
        ]}),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn set_exception_creates_a_full_day_off() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);

    let (status, body) = call(
        app_for(dir.path()),
        "/api/vets/availability/exceptions",
        Some(&vet.token),
        json!({"date": "2026-09-10", "is_available": false}),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["is_available"], false);
    assert!(body["start_time"].is_null());
}

#[tokio::test]
async fn set_exception_with_custom_hours_requires_both_times() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);

    let (status, _) = call(
        app_for(dir.path()),
        "/api/vets/availability/exceptions",
        Some(&vet.token),
        json!({"date": "2026-09-10", "is_available": true}),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn set_exception_unavailable_rejects_times_present() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);

    let (status, _) = call(
        app_for(dir.path()),
        "/api/vets/availability/exceptions",
        Some(&vet.token),
        json!({"date": "2026-09-10", "is_available": false, "start_time": "09:00:00.0"}),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn set_exception_twice_for_the_same_date_upserts() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);

    call(
        app_for(dir.path()),
        "/api/vets/availability/exceptions",
        Some(&vet.token),
        json!({"date": "2026-09-10", "is_available": false}),
    )
    .await;
    let (status, body) = call(
        app_for(dir.path()),
        "/api/vets/availability/exceptions",
        Some(&vet.token),
        json!({
            "date": "2026-09-10",
            "is_available": true,
            "start_time": "10:00:00.0",
            "end_time": "14:00:00.0",
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["is_available"], true);

    let exceptions = model::read_exceptions(dir.path(), vet.vet_id.unwrap()).unwrap();
    assert_eq!(exceptions.len(), 1);
    assert!(exceptions[0].is_available);
}
