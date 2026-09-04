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

fn owner_payload(username: &str) -> Value {
    json!({
        "username": username,
        "password": "correct horse",
        "type": "OWNER",
    })
}

fn vet_payload(username: &str) -> Value {
    json!({
        "username": username,
        "password": "correct horse",
        "type": "VET",
    })
}

#[tokio::test]
async fn register_owner_returns_201_with_id_username_and_role() {
    let (app, _dir) = new_app();

    let (status, body) = post_json(app, "/api/users/register", owner_payload("alice")).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["username"], "alice");
    assert_eq!(body["role"], "OWNER");
    assert!(body["id"].is_i64());
}

#[tokio::test]
async fn register_vet_returns_201() {
    let (app, _dir) = new_app();

    let (status, body) = post_json(app, "/api/users/register", vet_payload("dr-bob")).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["role"], "VET");
}

#[tokio::test]
async fn register_duplicate_username_is_rejected_with_conflict() {
    let (app, dir) = new_app();
    post_json(app, "/api/users/register", owner_payload("dup")).await;

    let app = router().with_state(Config::for_test(dir.path().to_path_buf()));
    let (status, body) = post_json(app, "/api/users/register", owner_payload("dup")).await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "USERNAME_TAKEN");
}

#[tokio::test]
async fn register_username_uniqueness_is_case_insensitive() {
    let (app, dir) = new_app();
    post_json(app, "/api/users/register", owner_payload("CaseUser")).await;

    let app = router().with_state(Config::for_test(dir.path().to_path_buf()));
    let (status, _) = post_json(app, "/api/users/register", owner_payload("caseuser")).await;

    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn register_admin_role_is_rejected() {
    let (app, _dir) = new_app();

    let mut payload = owner_payload("wannabe-admin");
    payload["type"] = json!("ADMIN");
    let (status, body) = post_json(app, "/api/users/register", payload).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn register_blank_username_is_rejected() {
    let (app, _dir) = new_app();

    let payload = owner_payload("   ");
    let (status, _) = post_json(app, "/api/users/register", payload).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn register_short_password_is_rejected() {
    let (app, _dir) = new_app();

    let mut payload = owner_payload("shortpw");
    payload["password"] = json!("short");
    let (status, _) = post_json(app, "/api/users/register", payload).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn register_then_login_returns_a_bearer_token() {
    let (app, dir) = new_app();
    post_json(app, "/api/users/register", owner_payload("login-user")).await;

    let app = router().with_state(Config::for_test(dir.path().to_path_buf()));
    let (status, body) = post_json(
        app,
        "/api/auth/login",
        json!({"username": "login-user", "password": "correct horse"}),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(body["token"].as_str().is_some_and(|t| !t.is_empty()));
    assert_eq!(body["type"], "Bearer");
}

#[tokio::test]
async fn login_with_wrong_password_is_unauthorized() {
    let (app, dir) = new_app();
    post_json(app, "/api/users/register", owner_payload("wrongpw-user")).await;

    let app = router().with_state(Config::for_test(dir.path().to_path_buf()));
    let (status, body) = post_json(
        app,
        "/api/auth/login",
        json!({"username": "wrongpw-user", "password": "not the right password"}),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "INVALID_CREDENTIALS");
}

#[tokio::test]
async fn login_with_unknown_username_is_unauthorized() {
    let (app, _dir) = new_app();

    let (status, body) = post_json(
        app,
        "/api/auth/login",
        json!({"username": "nobody", "password": "whatever1"}),
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
        "/api/users/register",
        owner_payload("deactivated-user"),
    )
    .await;

    // No deactivate endpoint exists yet (that's the admin slice) — flip the
    // flag directly on disk, the way that slice will eventually do it.
    let mut user = model::find_user_by_username(dir.path(), "deactivated-user")
        .unwrap()
        .unwrap();
    user.is_active = false;
    model::write_user(dir.path(), &user).unwrap();

    let app = router().with_state(Config::for_test(dir.path().to_path_buf()));
    let (status, body) = post_json(
        app,
        "/api/auth/login",
        json!({"username": "deactivated-user", "password": "correct horse"}),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "ACCOUNT_DEACTIVATED");
}

#[tokio::test]
async fn login_updates_last_login() {
    let (app, dir) = new_app();
    post_json(app, "/api/users/register", owner_payload("lastlogin-user")).await;

    let before = model::find_user_by_username(dir.path(), "lastlogin-user")
        .unwrap()
        .unwrap();
    assert!(before.last_login.is_none());

    let app = router().with_state(Config::for_test(dir.path().to_path_buf()));
    post_json(
        app,
        "/api/auth/login",
        json!({"username": "lastlogin-user", "password": "correct horse"}),
    )
    .await;

    let after = model::find_user_by_username(dir.path(), "lastlogin-user")
        .unwrap()
        .unwrap();
    assert!(after.last_login.is_some());
}

#[tokio::test]
async fn aboutme_returns_the_caller_from_their_bearer_token() {
    let (app, dir) = new_app();
    post_json(app, "/api/users/register", owner_payload("aboutme-user")).await;

    let app = router().with_state(Config::for_test(dir.path().to_path_buf()));
    let (_, login_body) = post_json(
        app,
        "/api/auth/login",
        json!({"username": "aboutme-user", "password": "correct horse"}),
    )
    .await;
    let token = login_body["token"].as_str().unwrap();

    let app = router().with_state(Config::for_test(dir.path().to_path_buf()));
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/users/aboutme")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["username"], "aboutme-user");
    assert_eq!(body["role"], "OWNER");
    assert!(body["id"].is_i64());
}
