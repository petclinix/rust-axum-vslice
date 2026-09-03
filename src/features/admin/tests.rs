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

use super::router;

struct Fixture {
    dir: tempfile::TempDir,
    config: Config,
    admin_token: String,
    owner_token: String,
    owner_user_id: Uuid,
}

impl Fixture {
    fn data_dir(&self) -> &Path {
        self.dir.path()
    }

    /// Merges in the `registration` router too — the activity-log tests
    /// below need `POST /api/auth/register` to actually be routed, not
    /// just the admin endpoints under test.
    fn app(&self) -> Router {
        axum::Router::new()
            .merge(router())
            .merge(crate::features::registration::router())
            .with_state(self.config.clone())
    }
}

fn seed() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let data_dir = dir.path();

    let admin_user_id = Uuid::new_v4();
    registration::write_user(
        data_dir,
        &registration::User {
            id: admin_user_id,
            email: "admin@petclinix.local".to_string(),
            password_hash: "unused".to_string(),
            role: Role::Admin,
            is_active: true,
            created_at: OffsetDateTime::now_utc(),
            last_login: None,
        },
    )
    .unwrap();
    let admin_token =
        token::issue(&config.jwt_secret, &admin_user_id.to_string(), Role::Admin).unwrap();

    let owner_user_id = Uuid::new_v4();
    registration::write_user(
        data_dir,
        &registration::User {
            id: owner_user_id,
            email: "owner@example.com".to_string(),
            password_hash: "unused".to_string(),
            role: Role::Owner,
            is_active: true,
            created_at: OffsetDateTime::now_utc(),
            last_login: None,
        },
    )
    .unwrap();
    let owner_token =
        token::issue(&config.jwt_secret, &owner_user_id.to_string(), Role::Owner).unwrap();

    Fixture {
        dir,
        config,
        admin_token,
        owner_token,
        owner_user_id,
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

#[tokio::test]
async fn list_users_requires_the_admin_role() {
    let fixture = seed();

    let (status, _) = call(
        fixture.app(),
        "GET",
        "/api/admin/users",
        Some(&fixture.owner_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn list_users_returns_every_registered_user() {
    let fixture = seed();

    let (status, body) = call(
        fixture.app(),
        "GET",
        "/api/admin/users",
        Some(&fixture.admin_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn deactivate_user_flips_is_active() {
    let fixture = seed();

    let (status, body) = call(
        fixture.app(),
        "POST",
        &format!("/api/admin/users/{}/deactivate", fixture.owner_user_id),
        Some(&fixture.admin_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["is_active"], false);

    let user = registration::read_user(fixture.data_dir(), fixture.owner_user_id)
        .unwrap()
        .unwrap();
    assert!(!user.is_active);
}

#[tokio::test]
async fn deactivate_user_requires_the_admin_role() {
    let fixture = seed();

    let (status, _) = call(
        fixture.app(),
        "POST",
        &format!("/api/admin/users/{}/deactivate", fixture.owner_user_id),
        Some(&fixture.owner_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn deactivate_a_missing_user_is_not_found() {
    let fixture = seed();

    let (status, _) = call(
        fixture.app(),
        "POST",
        &format!("/api/admin/users/{}/deactivate", Uuid::new_v4()),
        Some(&fixture.admin_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_stats_requires_the_admin_role() {
    let fixture = seed();

    let (status, _) = call(
        fixture.app(),
        "GET",
        "/api/admin/stats",
        Some(&fixture.owner_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn get_stats_returns_zero_counts_with_no_data() {
    let fixture = seed();

    let (status, body) = call(
        fixture.app(),
        "GET",
        "/api/admin/stats",
        Some(&fixture.admin_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total_pets"], 0);
}

#[tokio::test]
async fn list_activity_requires_the_admin_role() {
    let fixture = seed();

    let (status, _) = call(
        fixture.app(),
        "GET",
        "/api/admin/activity",
        Some(&fixture.owner_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn list_activity_reflects_events_recorded_by_other_slices() {
    let fixture = seed();

    // Registration itself records "user_registered".
    call(
        fixture.app(),
        "POST",
        "/api/auth/register",
        None,
        Some(json!({
            "email": "newowner@example.com",
            "password": "correct horse",
            "role": "owner",
            "name": "Charlie",
            "phone": "555-0111",
        })),
    )
    .await;

    let (status, body) = call(
        fixture.app(),
        "GET",
        "/api/admin/activity",
        Some(&fixture.admin_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let events = body.as_array().unwrap();
    assert!(
        events.iter().any(|e| e["event_type"] == "user_registered"),
        "expected a user_registered event, got {events:?}"
    );
}

#[tokio::test]
async fn list_activity_with_a_date_filter_returns_todays_events() {
    let fixture = seed();
    call(
        fixture.app(),
        "POST",
        "/api/auth/register",
        None,
        Some(json!({
            "email": "newowner2@example.com",
            "password": "correct horse",
            "role": "owner",
            "name": "Dana",
            "phone": "555-0112",
        })),
    )
    .await;

    // `Date` has no `Display` impl in this crate's config; reuse its
    // already-confirmed serde JSON encoding ("YYYY-MM-DD") for the query
    // string instead of guessing a format.
    let today = OffsetDateTime::now_utc().date();
    let today_str = serde_json::to_string(&today)
        .unwrap()
        .trim_matches('"')
        .to_string();
    let (status, body) = call(
        fixture.app(),
        "GET",
        &format!("/api/admin/activity?date={today_str}"),
        Some(&fixture.admin_token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(!body.as_array().unwrap().is_empty());
}
