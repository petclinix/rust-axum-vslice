use axum::Router;
use axum::body::Body;
use axum::http::header::AUTHORIZATION;
use axum::http::{Request, StatusCode};
use serde_json::Value;
use time::OffsetDateTime;
use tower::ServiceExt;
use uuid::Uuid;

use crate::auth::token;
use crate::config::Config;
use crate::domain::Role;
use crate::features::registration::model as registration;

use super::router;

fn app_for(config: &Config) -> Router {
    router().with_state(config.clone())
}

async fn call(app: Router, token: Option<&str>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("GET").uri("/api/vets");
    if let Some(t) = token {
        builder = builder.header(AUTHORIZATION, format!("Bearer {t}"));
    }
    let response = app
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
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
async fn list_vets_without_auth_is_unauthenticated() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());

    let (status, _) = call(app_for(&config), None).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_vets_requires_the_owner_role() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet_user_id = Uuid::new_v4();
    registration::write_user(
        dir.path(),
        &registration::User {
            id: vet_user_id,
            username: "vet@example.com".to_string(),
            password_hash: "unused".to_string(),
            role: Role::Vet,
            is_active: true,
            created_at: OffsetDateTime::now_utc(),
            last_login: None,
        },
    )
    .unwrap();
    let vet_token = token::issue(
        &config.jwt_secret,
        &vet_user_id.to_string(),
        "vet@example.com",
        Role::Vet,
    )
    .unwrap();

    let (status, _) = call(app_for(&config), Some(&vet_token)).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn list_vets_returns_every_vet_with_their_username() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());

    let owner_user_id = Uuid::new_v4();
    registration::write_user(
        dir.path(),
        &registration::User {
            id: owner_user_id,
            username: "owner@example.com".to_string(),
            password_hash: "unused".to_string(),
            role: Role::Owner,
            is_active: true,
            created_at: OffsetDateTime::now_utc(),
            last_login: None,
        },
    )
    .unwrap();
    let owner_token = token::issue(
        &config.jwt_secret,
        &owner_user_id.to_string(),
        "owner@example.com",
        Role::Owner,
    )
    .unwrap();

    let vet_user_id = Uuid::new_v4();
    registration::write_user(
        dir.path(),
        &registration::User {
            id: vet_user_id,
            username: "dr-bob".to_string(),
            password_hash: "unused".to_string(),
            role: Role::Vet,
            is_active: true,
            created_at: OffsetDateTime::now_utc(),
            last_login: None,
        },
    )
    .unwrap();
    registration::write_vet(
        dir.path(),
        &registration::Vet {
            id: Uuid::new_v4(),
            user_id: vet_user_id,
            name: "Dr. Bob".to_string(),
            specialty: "Surgery".to_string(),
        },
    )
    .unwrap();

    let (status, body) = call(app_for(&config), Some(&owner_token)).await;

    assert_eq!(status, StatusCode::OK);
    let vets = body.as_array().unwrap();
    assert_eq!(vets.len(), 1);
    assert_eq!(vets[0]["username"], "dr-bob");
    assert!(vets[0]["id"].is_i64());
}
