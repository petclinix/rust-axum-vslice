use std::path::Path;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::{AuthUser, password, token};
use crate::config::Config;
use crate::domain::{self, Role};
use crate::error::AppError;
use crate::features::admin::activity;
use crate::storage;

use super::model;

/// Matches the target contract's `RegisterRequest`
/// (`docs/petclinix-openapi-snapshot.json`) exactly — it carries no profile
/// fields (name/phone/specialty) beyond `username`/`password`/`type`, so
/// `register_locked` fills the internal `Owner`/`Vet` profile's `name` with
/// the username and leaves `phone`/`specialty` blank; nothing in the target
/// contract ever reads those back.
#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
    #[serde(rename = "type")]
    pub role: Role,
}

#[derive(Debug, Serialize)]
pub struct RegisterResponse {
    pub id: i64,
    pub username: String,
    pub role: Role,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub token: String,
    #[serde(rename = "type")]
    pub token_type: String,
}

/// `GET /api/users/aboutme`'s response shape — also what a login-response
/// envelope's `UserResponse` counterpart in the target contract looks like.
#[derive(Debug, Serialize)]
pub struct UserResponse {
    pub id: i64,
    pub username: String,
    pub role: Role,
}

fn validate_register_request(req: &RegisterRequest) -> Result<(), AppError> {
    if req.role == Role::Admin {
        return Err(AppError::Validation(
            "type must be OWNER or VET".to_string(),
        ));
    }
    if req.username.trim().is_empty() {
        return Err(AppError::Validation("username is required".to_string()));
    }
    if req.password.len() < 8 {
        return Err(AppError::Validation(
            "password must be at least 8 characters".to_string(),
        ));
    }
    Ok(())
}

pub async fn register(
    State(config): State<Config>,
    Json(req): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<RegisterResponse>), AppError> {
    validate_register_request(&req)?;

    let data_dir = config.data_dir.clone();
    let response = tokio::task::spawn_blocking(move || register_locked(&data_dir, req))
        .await
        .map_err(|_| AppError::Internal)??;

    Ok((StatusCode::CREATED, Json(response)))
}

/// Runs inside `spawn_blocking`: takes the exclusive `users.lock` and does
/// the read-check-then-write critical section synchronously — file locks
/// are blocking OS calls, so this must not run on an async task directly,
/// the same way any other blocking I/O shouldn't (`docs/architecture-internals.md` §2).
fn register_locked(data_dir: &Path, req: RegisterRequest) -> Result<RegisterResponse, AppError> {
    let _lock = storage::FileLock::exclusive(&model::users_lock_path(data_dir))?;

    if model::find_user_by_username(data_dir, &req.username)?.is_some() {
        return Err(AppError::UsernameTaken);
    }

    let password_hash = password::hash(&req.password).map_err(|_| AppError::Internal)?;

    let user = model::User {
        id: Uuid::new_v4(),
        username: req.username.clone(),
        password_hash,
        role: req.role,
        is_active: true,
        created_at: OffsetDateTime::now_utc(),
        last_login: None,
    };
    model::write_user(data_dir, &user)?;

    match req.role {
        Role::Owner => {
            let owner = model::Owner {
                id: Uuid::new_v4(),
                user_id: user.id,
                name: req.username.clone(),
                phone: String::new(),
            };
            model::write_owner(data_dir, &owner)?;
        }
        Role::Vet => {
            let vet = model::Vet {
                id: Uuid::new_v4(),
                user_id: user.id,
                name: req.username.clone(),
                specialty: String::new(),
            };
            model::write_vet(data_dir, &vet)?;
        }
        Role::Admin => unreachable!("rejected in validate_register_request"),
    }

    // Best-effort: a logging failure shouldn't fail a registration that
    // already succeeded — activity is a thin side utility, not part of the
    // transaction it observes.
    if let Err(e) = activity::record(
        data_dir,
        &user.username,
        "user_registered",
        serde_json::json!({"user_id": user.id, "role": user.role}),
    ) {
        tracing::warn!(error = %e, "failed to record activity log entry");
    }

    Ok(RegisterResponse {
        id: domain::wire_id(user.id),
        username: user.username,
        role: user.role,
    })
}

pub async fn login(
    State(config): State<Config>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, AppError> {
    let data_dir = config.data_dir.clone();
    let user = tokio::task::spawn_blocking(move || login_locked(&data_dir, req))
        .await
        .map_err(|_| AppError::Internal)??;

    let token = token::issue(&config.jwt_secret, &user.id.to_string(), user.role)
        .map_err(|_| AppError::Internal)?;

    Ok(Json(LoginResponse {
        token,
        token_type: "Bearer".to_string(),
    }))
}

/// Same blocking-task rationale as `register_locked`. Only the lookup is
/// taken under a lock (shared — a read); the `last_login` write afterward is
/// a plain single-record update with no invariant to protect, so it doesn't
/// need one (see `docs/architecture.md`'s Design Constraints).
fn login_locked(data_dir: &Path, req: LoginRequest) -> Result<model::User, AppError> {
    let found = {
        let _lock = storage::FileLock::shared(&model::users_lock_path(data_dir))?;
        model::find_user_by_username(data_dir, &req.username)?
    };

    let mut user = found.ok_or(AppError::InvalidCredentials)?;

    let password_ok =
        password::verify(&req.password, &user.password_hash).map_err(|_| AppError::Internal)?;
    if !password_ok {
        return Err(AppError::InvalidCredentials);
    }

    // Checked only after a successful password match, so a request with the
    // wrong password can't be used to probe whether a username belongs to a
    // deactivated account.
    if !user.is_active {
        return Err(AppError::AccountDeactivated);
    }

    user.last_login = Some(OffsetDateTime::now_utc());
    model::write_user(data_dir, &user)?;

    if let Err(e) = activity::record(
        data_dir,
        &user.username,
        "user_login",
        serde_json::json!({"user_id": user.id}),
    ) {
        tracing::warn!(error = %e, "failed to record activity log entry");
    }

    Ok(user)
}

/// `GET /api/users/aboutme` — "who am I", from the caller's own verified
/// JWT. The token carries `sub`/`role` only (`auth::token::Claims`), so
/// this still needs one read to fill in `username`.
pub async fn aboutme(
    State(config): State<Config>,
    auth: AuthUser,
) -> Result<Json<UserResponse>, AppError> {
    let data_dir = config.data_dir.clone();
    let user = tokio::task::spawn_blocking(move || model::read_user(&data_dir, auth.id))
        .await
        .map_err(|_| AppError::Internal)??
        .ok_or(AppError::Unauthenticated)?;

    Ok(Json(UserResponse {
        id: domain::wire_id(user.id),
        username: user.username,
        role: user.role,
    }))
}
