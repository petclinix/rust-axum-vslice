use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::config::Config;

use super::model;
use super::router;

fn new_app() -> (Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    (router().with_state(config), dir)
}

async fn post_json(app: Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap();
    (status, json)
}

fn owner_payload(email: &str) -> Value {
    json!({
        "email": email,
        "password": "correct horse",
        "role": "owner",
        "name": "Alice",
        "phone": "555-0100",
    })
}

fn vet_payload(email: &str) -> Value {
    json!({
        "email": email,
        "password": "correct horse",
        "role": "vet",
        "name": "Dr. Bob",
        "specialty": "Surgery",
    })
}

#[tokio::test]
async fn register_owner_returns_201_with_id_email_and_role() {
    let (app, _dir) = new_app();

    let (status, body) = post_json(app, "/api/auth/register", owner_payload("a@example.com")).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["email"], "a@example.com");
    assert_eq!(body["role"], "owner");
    assert!(body["id"].is_string());
}

#[tokio::test]
async fn register_vet_returns_201() {
    let (app, _dir) = new_app();

    let (status, body) = post_json(app, "/api/auth/register", vet_payload("vet@example.com")).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["role"], "vet");
}

#[tokio::test]
async fn register_duplicate_email_is_rejected_with_conflict() {
    let (app, dir) = new_app();
    post_json(app, "/api/auth/register", owner_payload("dup@example.com")).await;

    let app = router().with_state(Config::for_test(dir.path().to_path_buf()));
    let (status, body) =
        post_json(app, "/api/auth/register", owner_payload("dup@example.com")).await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "EMAIL_TAKEN");
}

#[tokio::test]
async fn register_email_uniqueness_is_case_insensitive() {
    let (app, dir) = new_app();
    post_json(app, "/api/auth/register", owner_payload("Case@Example.com")).await;

    let app = router().with_state(Config::for_test(dir.path().to_path_buf()));
    let (status, _) = post_json(app, "/api/auth/register", owner_payload("case@example.com")).await;

    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn register_admin_role_is_rejected() {
    let (app, _dir) = new_app();

    let mut payload = owner_payload("admin@example.com");
    payload["role"] = json!("admin");
    let (status, body) = post_json(app, "/api/auth/register", payload).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn register_owner_without_phone_is_rejected() {
    let (app, _dir) = new_app();

    let mut payload = owner_payload("nophone@example.com");
    payload.as_object_mut().unwrap().remove("phone");
    let (status, _) = post_json(app, "/api/auth/register", payload).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn register_vet_without_specialty_is_rejected() {
    let (app, _dir) = new_app();

    let mut payload = vet_payload("nospecialty@example.com");
    payload.as_object_mut().unwrap().remove("specialty");
    let (status, _) = post_json(app, "/api/auth/register", payload).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn register_short_password_is_rejected() {
    let (app, _dir) = new_app();

    let mut payload = owner_payload("shortpw@example.com");
    payload["password"] = json!("short");
    let (status, _) = post_json(app, "/api/auth/register", payload).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn register_then_login_returns_a_bearer_token() {
    let (app, dir) = new_app();
    post_json(
        app,
        "/api/auth/register",
        owner_payload("login@example.com"),
    )
    .await;

    let app = router().with_state(Config::for_test(dir.path().to_path_buf()));
    let (status, body) = post_json(
        app,
        "/api/auth/login",
        json!({"email": "login@example.com", "password": "correct horse"}),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(body["token"].as_str().is_some_and(|t| !t.is_empty()));
}

#[tokio::test]
async fn login_with_wrong_password_is_unauthorized() {
    let (app, dir) = new_app();
    post_json(
        app,
        "/api/auth/register",
        owner_payload("wrongpw@example.com"),
    )
    .await;

    let app = router().with_state(Config::for_test(dir.path().to_path_buf()));
    let (status, body) = post_json(
        app,
        "/api/auth/login",
        json!({"email": "wrongpw@example.com", "password": "not the right password"}),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "INVALID_CREDENTIALS");
}

#[tokio::test]
async fn login_with_unknown_email_is_unauthorized() {
    let (app, _dir) = new_app();

    let (status, body) = post_json(
        app,
        "/api/auth/login",
        json!({"email": "nobody@example.com", "password": "whatever1"}),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "INVALID_CREDENTIALS");
}

#[tokio::test]
async fn login_on_a_deactivated_account_is_forbidden() {
    let (app, dir) = new_app();
    post_json(
        app,
        "/api/auth/register",
        owner_payload("deactivated@example.com"),
    )
    .await;

    // No deactivate endpoint exists yet (that's the admin slice) — flip the
    // flag directly on disk, the way that slice will eventually do it.
    let mut user = model::find_user_by_email(dir.path(), "deactivated@example.com")
        .unwrap()
        .unwrap();
    user.is_active = false;
    model::write_user(dir.path(), &user).unwrap();

    let app = router().with_state(Config::for_test(dir.path().to_path_buf()));
    let (status, body) = post_json(
        app,
        "/api/auth/login",
        json!({"email": "deactivated@example.com", "password": "correct horse"}),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "ACCOUNT_DEACTIVATED");
}

#[tokio::test]
async fn login_updates_last_login() {
    let (app, dir) = new_app();
    post_json(
        app,
        "/api/auth/register",
        owner_payload("lastlogin@example.com"),
    )
    .await;

    let before = model::find_user_by_email(dir.path(), "lastlogin@example.com")
        .unwrap()
        .unwrap();
    assert!(before.last_login.is_none());

    let app = router().with_state(Config::for_test(dir.path().to_path_buf()));
    post_json(
        app,
        "/api/auth/login",
        json!({"email": "lastlogin@example.com", "password": "correct horse"}),
    )
    .await;

    let after = model::find_user_by_email(dir.path(), "lastlogin@example.com")
        .unwrap()
        .unwrap();
    assert!(after.last_login.is_some());
}
