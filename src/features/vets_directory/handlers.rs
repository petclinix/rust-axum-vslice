use std::path::Path;

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::auth::AuthUser;
use crate::config::Config;
use crate::domain::{self, Role};
use crate::error::AppError;
use crate::features::registration::model as registration;

/// Matches the target contract's `Vet` exactly (`docs/petclinix-openapi-
/// snapshot.json`) — just `id`/`username`, not the richer `name`/
/// `specialty` profile this repo used to expose. That profile data still
/// exists internally (`registration::Vet`); nothing in the target contract
/// ever reads it back.
#[derive(Debug, Serialize)]
pub struct VetResponse {
    pub id: i64,
    pub username: String,
}

pub async fn list_vets(
    State(config): State<Config>,
    auth: AuthUser,
) -> Result<Json<Vec<VetResponse>>, AppError> {
    if auth.role != Role::Owner {
        return Err(AppError::Forbidden(
            "this endpoint requires the owner role".to_string(),
        ));
    }

    let data_dir = config.data_dir.clone();
    let vets = tokio::task::spawn_blocking(move || list_vets_blocking(&data_dir))
        .await
        .map_err(|_| AppError::Internal)??;

    Ok(Json(vets))
}

fn list_vets_blocking(data_dir: &Path) -> Result<Vec<VetResponse>, AppError> {
    registration::list_all_vets(data_dir)?
        .into_iter()
        .map(|vet| {
            let user = registration::read_user(data_dir, vet.user_id)?
                .ok_or_else(|| AppError::NotFound("vet not found".to_string()))?;
            Ok(VetResponse {
                id: domain::wire_id(vet.id),
                username: user.username,
            })
        })
        .collect()
}
