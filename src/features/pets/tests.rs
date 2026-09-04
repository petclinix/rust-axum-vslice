use std::path::Path;

use axum::Router;
use axum::body::Body;
use axum::http::header::AUTHORIZATION;
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tower::ServiceExt;
use uuid::Uuid;

use crate::auth::token;
use crate::config::Config;
use crate::domain::Role;
use crate::features::registration::model as registration;

use super::router;

/// A user + (for owners) an Owner record seeded directly through
/// `registration::model` — pets tests shouldn't depend on the registration
/// slice's HTTP layer, only on the cross-slice lookup it exposes
/// (`docs/architecture.md` Design Constraint 5) — plus a JWT for that user.
struct SeededUser {
    token: String,
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

    if role == Role::Owner {
        registration::write_owner(
            data_dir,
            &registration::Owner {
                id: Uuid::new_v4(),
                user_id,
                name: "Alice".to_string(),
                phone: "555-0100".to_string(),
            },
        )
        .unwrap();
    }

    let token = token::issue(jwt_secret, &user_id.to_string(), role).unwrap();
    SeededUser { token }
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
    // A malformed-body rejection from axum's `Json` extractor itself (e.g.
    // an unknown enum variant) never reaches `AppError` — it's plain text,
    // not the usual `{error, code}` shape, so fall back to it as a string
    // rather than unwrapping a JSON parse that was never going to succeed.
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, json)
}

fn pet_payload(name: &str) -> Value {
    json!({
        "name": name,
        "species": "DOG",
        "breed": "Labrador",
        "gender": "MALE",
        "birthDate": "2020-01-15",
        "picture": BASE64.encode(b"fake image bytes"),
        "pictureContentType": "image/jpeg",
    })
}

#[tokio::test]
async fn add_pet_without_auth_is_unauthenticated() {
    let dir = tempfile::tempdir().unwrap();

    let (status, _) = call(
        app_for(dir.path()),
        "POST",
        "/api/pets",
        None,
        Some(pet_payload("Rex")),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn vet_role_is_forbidden_from_pets_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let vet = seed_user(dir.path(), &config.jwt_secret, Role::Vet);

    let (status, _) = call(
        app_for(dir.path()),
        "GET",
        "/api/pets",
        Some(&vet.token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn add_pet_returns_201_with_the_decoded_picture_re_encoded() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let owner = seed_user(dir.path(), &config.jwt_secret, Role::Owner);

    let (status, body) = call(
        app_for(dir.path()),
        "POST",
        "/api/pets",
        Some(&owner.token),
        Some(pet_payload("Rex")),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["name"], "Rex");
    assert_eq!(body["species"], "DOG");
    assert_eq!(body["gender"], "MALE");
    assert_eq!(body["pictureContentType"], "image/jpeg");
    assert_eq!(body["active"], true);
    assert!(body["id"].is_i64());
    assert_eq!(
        BASE64.decode(body["picture"].as_str().unwrap()).unwrap(),
        b"fake image bytes"
    );
}

#[tokio::test]
async fn add_pet_defaults_species_and_gender_when_omitted() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let owner = seed_user(dir.path(), &config.jwt_secret, Role::Owner);
    let mut payload = pet_payload("Rex");
    payload.as_object_mut().unwrap().remove("species");
    payload.as_object_mut().unwrap().remove("gender");

    let (status, body) = call(
        app_for(dir.path()),
        "POST",
        "/api/pets",
        Some(&owner.token),
        Some(payload),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["species"], "OTHER");
    assert_eq!(body["gender"], "UNKNOWN");
}

#[tokio::test]
async fn add_pet_with_an_unknown_species_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let owner = seed_user(dir.path(), &config.jwt_secret, Role::Owner);
    let mut payload = pet_payload("Rex");
    payload["species"] = json!("DRAGON");

    let (status, _) = call(
        app_for(dir.path()),
        "POST",
        "/api/pets",
        Some(&owner.token),
        Some(payload),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn add_pet_with_unsupported_picture_type_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let owner = seed_user(dir.path(), &config.jwt_secret, Role::Owner);
    let mut payload = pet_payload("Rex");
    payload["pictureContentType"] = json!("application/pdf");

    let (status, body) = call(
        app_for(dir.path()),
        "POST",
        "/api/pets",
        Some(&owner.token),
        Some(payload),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn add_pet_without_a_name_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let owner = seed_user(dir.path(), &config.jwt_secret, Role::Owner);
    let mut payload = pet_payload("");
    payload["name"] = json!("   ");

    let (status, _) = call(
        app_for(dir.path()),
        "POST",
        "/api/pets",
        Some(&owner.token),
        Some(payload),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_pets_only_returns_the_caller_s_own_pets() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let alice = seed_user(dir.path(), &config.jwt_secret, Role::Owner);
    let bob = seed_user(dir.path(), &config.jwt_secret, Role::Owner);

    call(
        app_for(dir.path()),
        "POST",
        "/api/pets",
        Some(&alice.token),
        Some(pet_payload("Rex")),
    )
    .await;
    call(
        app_for(dir.path()),
        "POST",
        "/api/pets",
        Some(&bob.token),
        Some(pet_payload("Whiskers")),
    )
    .await;

    let (status, body) = call(
        app_for(dir.path()),
        "GET",
        "/api/pets",
        Some(&alice.token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let pets = body.as_array().unwrap();
    assert_eq!(pets.len(), 1);
    assert_eq!(pets[0]["name"], "Rex");
}

#[tokio::test]
async fn get_pet_returns_it() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let owner = seed_user(dir.path(), &config.jwt_secret, Role::Owner);
    let (_, created) = call(
        app_for(dir.path()),
        "POST",
        "/api/pets",
        Some(&owner.token),
        Some(pet_payload("Rex")),
    )
    .await;
    let pet_id = created["id"].as_i64().unwrap();

    let (status, body) = call(
        app_for(dir.path()),
        "GET",
        &format!("/api/pets/{pet_id}"),
        Some(&owner.token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], pet_id);
}

#[tokio::test]
async fn get_missing_pet_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let owner = seed_user(dir.path(), &config.jwt_secret, Role::Owner);

    let (status, body) = call(
        app_for(dir.path()),
        "GET",
        "/api/pets/999999999",
        Some(&owner.token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "NOT_FOUND");
}

#[tokio::test]
async fn get_pet_owned_by_someone_else_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let alice = seed_user(dir.path(), &config.jwt_secret, Role::Owner);
    let bob = seed_user(dir.path(), &config.jwt_secret, Role::Owner);
    let (_, created) = call(
        app_for(dir.path()),
        "POST",
        "/api/pets",
        Some(&alice.token),
        Some(pet_payload("Rex")),
    )
    .await;
    let pet_id = created["id"].as_i64().unwrap();

    let (status, body) = call(
        app_for(dir.path()),
        "GET",
        &format!("/api/pets/{pet_id}"),
        Some(&bob.token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "NOT_FOUND");
}

#[tokio::test]
async fn update_pet_changes_fields_and_picture() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let owner = seed_user(dir.path(), &config.jwt_secret, Role::Owner);
    let (_, created) = call(
        app_for(dir.path()),
        "POST",
        "/api/pets",
        Some(&owner.token),
        Some(pet_payload("Rex")),
    )
    .await;
    let pet_id = created["id"].as_i64().unwrap();

    let mut update = pet_payload("Rex the Second");
    update["picture"] = json!(BASE64.encode(b"updated bytes"));

    let (status, body) = call(
        app_for(dir.path()),
        "PUT",
        &format!("/api/pets/{pet_id}"),
        Some(&owner.token),
        Some(update),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "Rex the Second");
    assert_eq!(
        BASE64.decode(body["picture"].as_str().unwrap()).unwrap(),
        b"updated bytes"
    );
}

#[tokio::test]
async fn delete_pet_deactivates_it() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let owner = seed_user(dir.path(), &config.jwt_secret, Role::Owner);
    let (_, created) = call(
        app_for(dir.path()),
        "POST",
        "/api/pets",
        Some(&owner.token),
        Some(pet_payload("Rex")),
    )
    .await;
    let pet_id = created["id"].as_i64().unwrap();

    let (status, _) = call(
        app_for(dir.path()),
        "DELETE",
        &format!("/api/pets/{pet_id}"),
        Some(&owner.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, body) = call(
        app_for(dir.path()),
        "GET",
        &format!("/api/pets/{pet_id}"),
        Some(&owner.token),
        None,
    )
    .await;
    assert_eq!(body["active"], false);
}

#[tokio::test]
async fn delete_pet_owned_by_someone_else_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let alice = seed_user(dir.path(), &config.jwt_secret, Role::Owner);
    let bob = seed_user(dir.path(), &config.jwt_secret, Role::Owner);
    let (_, created) = call(
        app_for(dir.path()),
        "POST",
        "/api/pets",
        Some(&alice.token),
        Some(pet_payload("Rex")),
    )
    .await;
    let pet_id = created["id"].as_i64().unwrap();

    let (status, _) = call(
        app_for(dir.path()),
        "DELETE",
        &format!("/api/pets/{pet_id}"),
        Some(&bob.token),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn update_pet_owned_by_someone_else_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_test(dir.path().to_path_buf());
    let alice = seed_user(dir.path(), &config.jwt_secret, Role::Owner);
    let bob = seed_user(dir.path(), &config.jwt_secret, Role::Owner);
    let (_, created) = call(
        app_for(dir.path()),
        "POST",
        "/api/pets",
        Some(&alice.token),
        Some(pet_payload("Rex")),
    )
    .await;
    let pet_id = created["id"].as_i64().unwrap();

    let (status, _) = call(
        app_for(dir.path()),
        "PUT",
        &format!("/api/pets/{pet_id}"),
        Some(&bob.token),
        Some(pet_payload("Hijacked")),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}
