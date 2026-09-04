//! List/deactivate/activate any user.

use std::path::Path;

use axum::Json;
use axum::extract::{Path as PathParam, State};
use serde::Serialize;
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::config::Config;
use crate::domain::{self, Role};
use crate::error::AppError;
use crate::features::registration::model as registration;

use super::activity;

/// Matches the target contract's `AdminUserResponse` exactly
/// (`docs/petclinix-openapi-snapshot.json`) — `active`, not `is_active`;
/// no `createdAt` (the target schema doesn't carry one).
#[derive(Debug, Serialize)]
pub struct AdminUserResponse {
    pub id: i64,
    pub username: String,
    pub role: Role,
    pub active: bool,
    #[serde(rename = "lastLogin", with = "time::serde::rfc3339::option")]
    pub last_login: Option<OffsetDateTime>,
}

impl From<registration::User> for AdminUserResponse {
    fn from(u: registration::User) -> Self {
        Self {
            id: domain::wire_id(u.id),
            username: u.username,
            role: u.role,
            active: u.is_active,
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

fn user_not_found() -> AppError {
    AppError::NotFound("user not found".to_string())
}

pub async fn list_users(
    State(config): State<Config>,
    auth: AuthUser,
) -> Result<Json<Vec<AdminUserResponse>>, AppError> {
    require_admin(&auth)?;

    let data_dir = config.data_dir.clone();
    let users = tokio::task::spawn_blocking(move || registration::list_all_users(&data_dir))
        .await
        .map_err(|_| AppError::Internal)??;

    Ok(Json(
        users.into_iter().map(AdminUserResponse::from).collect(),
    ))
}

pub async fn deactivate_user(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(id): PathParam<i64>,
) -> Result<Json<AdminUserResponse>, AppError> {
    require_admin(&auth)?;
    set_active(config, auth.id, id, false, "user_deactivated").await
}

pub async fn activate_user(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(id): PathParam<i64>,
) -> Result<Json<AdminUserResponse>, AppError> {
    require_admin(&auth)?;
    set_active(config, auth.id, id, true, "user_activated").await
}

async fn set_active(
    config: Config,
    admin_user_id: Uuid,
    id: i64,
    active: bool,
    event_type: &'static str,
) -> Result<Json<AdminUserResponse>, AppError> {
    let data_dir = config.data_dir.clone();
    let user = tokio::task::spawn_blocking(move || {
        set_active_blocking(&data_dir, admin_user_id, id, active, event_type)
    })
    .await
    .map_err(|_| AppError::Internal)??;

    Ok(Json(user.into()))
}

/// Idempotent — (de)activating an already-(de)activated user is a harmless
/// no-op re-write, not an error.
fn set_active_blocking(
    data_dir: &Path,
    admin_user_id: Uuid,
    wire_id: i64,
    active: bool,
    event_type: &str,
) -> Result<registration::User, AppError> {
    let mut user =
        registration::find_user_by_wire_id(data_dir, wire_id)?.ok_or_else(user_not_found)?;
    user.is_active = active;
    registration::write_user(data_dir, &user)?;

    let admin_username = registration::read_user(data_dir, admin_user_id)?
        .map(|u| u.username)
        .unwrap_or_default();
    if let Err(e) = activity::record(
        data_dir,
        &admin_username,
        event_type,
        json!({"user_id": user.id}),
    ) {
        tracing::warn!(error = %e, "failed to record activity log entry");
    }

    Ok(user)
}
