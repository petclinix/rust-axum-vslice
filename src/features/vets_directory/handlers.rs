use axum::Json;
use axum::extract::State;
use serde::Serialize;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::config::Config;
use crate::domain::Role;
use crate::error::AppError;
use crate::features::registration::model as registration;

#[derive(Debug, Serialize)]
pub struct VetResponse {
    pub id: Uuid,
    pub name: String,
    pub specialty: String,
}

impl From<registration::Vet> for VetResponse {
    fn from(v: registration::Vet) -> Self {
        Self {
            id: v.id,
            name: v.name,
            specialty: v.specialty,
        }
    }
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
    let vets = tokio::task::spawn_blocking(move || registration::list_all_vets(&data_dir))
        .await
        .map_err(|_| AppError::Internal)??;

    Ok(Json(vets.into_iter().map(VetResponse::from).collect()))
}
