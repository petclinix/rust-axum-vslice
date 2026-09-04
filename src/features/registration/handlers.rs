use std::path::Path;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::{password, token};
use crate::config::Config;
use crate::domain::Role;
use crate::error::AppError;
use crate::features::admin::activity;
use crate::storage;

use super::model;

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    pub role: Role,
    pub name: String,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub specialty: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RegisterResponse {
    pub id: Uuid,
    pub email: String,
    pub role: Role,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub token: String,
}

fn validate_register_request(req: &RegisterRequest) -> Result<(), AppError> {
    if req.role == Role::Admin {
        return Err(AppError::Validation(
            "role must be owner or vet".to_string(),
        ));
    }
    if !req.email.contains('@') || req.email.trim().is_empty() {
        return Err(AppError::Validation("email is invalid".to_string()));
    }
    if req.password.len() < 8 {
        return Err(AppError::Validation(
            "password must be at least 8 characters".to_string(),
        ));
    }
    if req.name.trim().is_empty() {
        return Err(AppError::Validation("name is required".to_string()));
    }
    match req.role {
        Role::Owner => {
            if req.phone.as_deref().unwrap_or("").trim().is_empty() {
                return Err(AppError::Validation(
                    "phone is required for owners".to_string(),
                ));
            }
        }
        Role::Vet => {
            if req.specialty.as_deref().unwrap_or("").trim().is_empty() {
                return Err(AppError::Validation(
                    "specialty is required for vets".to_string(),
                ));
            }
        }
        Role::Admin => unreachable!("rejected above"),
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

    if model::find_user_by_email(data_dir, &req.email)?.is_some() {
        return Err(AppError::EmailTaken);
    }

    let password_hash = password::hash(&req.password).map_err(|_| AppError::Internal)?;

    let user = model::User {
        id: Uuid::new_v4(),
        email: req.email.clone(),
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
                name: req.name,
                phone: req.phone.unwrap_or_default(),
            };
            model::write_owner(data_dir, &owner)?;
        }
        Role::Vet => {
            let vet = model::Vet {
                id: Uuid::new_v4(),
                user_id: user.id,
                name: req.name,
                specialty: req.specialty.unwrap_or_default(),
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
        "user_registered",
        serde_json::json!({"user_id": user.id, "role": user.role}),
    ) {
        tracing::warn!(error = %e, "failed to record activity log entry");
    }

    Ok(RegisterResponse {
        id: user.id,
        email: user.email,
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

    Ok(Json(LoginResponse { token }))
}

/// Same blocking-task rationale as `register_locked`. Only the lookup is
/// taken under a lock (shared — a read); the `last_login` write afterward is
/// a plain single-record update with no invariant to protect, so it doesn't
/// need one (see `docs/architecture.md`'s Design Constraints).
fn login_locked(data_dir: &Path, req: LoginRequest) -> Result<model::User, AppError> {
    let found = {
        let _lock = storage::FileLock::shared(&model::users_lock_path(data_dir))?;
        model::find_user_by_email(data_dir, &req.email)?
    };

    let mut user = found.ok_or(AppError::InvalidCredentials)?;

    let password_ok =
        password::verify(&req.password, &user.password_hash).map_err(|_| AppError::Internal)?;
    if !password_ok {
        return Err(AppError::InvalidCredentials);
    }

    // Checked only after a successful password match, so a request with the
    // wrong password can't be used to probe whether an email belongs to a
    // deactivated account.
    if !user.is_active {
        return Err(AppError::AccountDeactivated);
    }

    user.last_login = Some(OffsetDateTime::now_utc());
    model::write_user(data_dir, &user)?;

    if let Err(e) = activity::record(
        data_dir,
        "user_login",
        serde_json::json!({"user_id": user.id}),
    ) {
        tracing::warn!(error = %e, "failed to record activity log entry");
    }

    Ok(user)
}
