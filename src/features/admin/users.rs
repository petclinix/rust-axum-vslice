//! List/deactivate any user (PLAN.md §6).

use std::path::Path;

use axum::Json;
use axum::extract::{Path as PathParam, State};
use serde::Serialize;
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::config::Config;
use crate::domain::Role;
use crate::error::AppError;
use crate::features::registration::model as registration;

use super::activity;

#[derive(Debug, Serialize)]
pub struct UserResponse {
    pub id: Uuid,
    pub email: String,
    pub role: Role,
    pub is_active: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_login: Option<OffsetDateTime>,
}

impl From<registration::User> for UserResponse {
    fn from(u: registration::User) -> Self {
        Self {
            id: u.id,
            email: u.email,
            role: u.role,
            is_active: u.is_active,
            created_at: u.created_at,
            last_login: u.last_login,
        }
    }
}

fn require_admin(auth: &AuthUser) -> Result<(), AppError> {
    if auth.role != Role::Admin {
        return Err(AppError::Forbidden(
            "this endpoint requires the admin role".to_string(),
        ));
    }
    Ok(())
}

pub async fn list_users(
    State(config): State<Config>,
    auth: AuthUser,
) -> Result<Json<Vec<UserResponse>>, AppError> {
    require_admin(&auth)?;

    let data_dir = config.data_dir.clone();
    let users = tokio::task::spawn_blocking(move || registration::list_all_users(&data_dir))
        .await
        .map_err(|_| AppError::Internal)??;

    Ok(Json(users.into_iter().map(UserResponse::from).collect()))
}

pub async fn deactivate_user(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(user_id): PathParam<Uuid>,
) -> Result<Json<UserResponse>, AppError> {
    require_admin(&auth)?;

    let data_dir = config.data_dir.clone();
    let user = tokio::task::spawn_blocking(move || deactivate_user_blocking(&data_dir, user_id))
        .await
        .map_err(|_| AppError::Internal)??;

    Ok(Json(user.into()))
}

/// Idempotent — deactivating an already-deactivated user is a harmless
/// no-op re-write, not an error.
fn deactivate_user_blocking(
    data_dir: &Path,
    user_id: Uuid,
) -> Result<registration::User, AppError> {
    let mut user = registration::read_user(data_dir, user_id)?
        .ok_or_else(|| AppError::NotFound("user not found".to_string()))?;
    user.is_active = false;
    registration::write_user(data_dir, &user)?;

    if let Err(e) = activity::record(data_dir, "user_deactivated", json!({"user_id": user_id})) {
        tracing::warn!(error = %e, "failed to record activity log entry");
    }

    Ok(user)
}
