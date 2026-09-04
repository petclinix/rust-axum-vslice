use std::path::Path;

use axum::Json;
use axum::extract::{Path as PathParam, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::config::Config;
use crate::domain::{AppointmentStatus, Role};
use crate::error::AppError;
use crate::features::appointments::model as appointments;
use crate::features::pets::model as pets;
use crate::features::registration::model as registration;

use super::model::{self, VisitType};

#[derive(Debug, Deserialize)]
pub struct RecordVisitRequest {
    #[serde(rename = "type")]
    pub visit_type: VisitType,
    pub remark: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct VisitResponse {
    pub id: Uuid,
    pub appointment_id: Uuid,
    #[serde(rename = "type")]
    pub visit_type: VisitType,
    pub remark: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl From<model::Visit> for VisitResponse {
    fn from(v: model::Visit) -> Self {
        Self {
            id: v.id,
            appointment_id: v.appointment_id,
            visit_type: v.visit_type,
            remark: v.remark,
            created_at: v.created_at,
        }
    }
}

fn resolve_vet_id(data_dir: &Path, user_id: Uuid) -> Result<Uuid, AppError> {
    let vet = registration::find_vet_by_user_id(data_dir, user_id)?
        .ok_or_else(|| AppError::Forbidden("no vet profile for this account".to_string()))?;
    Ok(vet.id)
}

fn resolve_owner_id(data_dir: &Path, user_id: Uuid) -> Result<Uuid, AppError> {
    let owner = registration::find_owner_by_user_id(data_dir, user_id)?
        .ok_or_else(|| AppError::Forbidden("no owner profile for this account".to_string()))?;
    Ok(owner.id)
}

fn pet_not_found() -> AppError {
    AppError::NotFound("pet not found".to_string())
}

pub async fn record_visit(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(appointment_id): PathParam<Uuid>,
    Json(req): Json<RecordVisitRequest>,
) -> Result<(StatusCode, Json<VisitResponse>), AppError> {
    if auth.role != Role::Vet {
        return Err(AppError::Forbidden(
            "this endpoint requires the vet role".to_string(),
        ));
    }
    if req.remark.trim().is_empty() {
        return Err(AppError::Validation("remark is required".to_string()));
    }

    let data_dir = config.data_dir.clone();
    let visit = tokio::task::spawn_blocking(move || {
        record_visit_blocking(&data_dir, auth.id, appointment_id, req)
    })
    .await
    .map_err(|_| AppError::Internal)??;

    Ok((StatusCode::CREATED, Json(visit.into())))
}

/// No lock here (see `docs/architecture.md`'s Design Constraints): a completed
/// appointment is recorded by exactly the one vet who just completed it,
/// with no realistic concurrent contention the way booking has — a plain
/// read-check-then-write is enough, same reasoning as `login`'s
/// `last_login` update.
fn record_visit_blocking(
    data_dir: &Path,
    user_id: Uuid,
    appointment_id: Uuid,
    req: RecordVisitRequest,
) -> Result<model::Visit, AppError> {
    let vet_id = resolve_vet_id(data_dir, user_id)?;
    let appointment = appointments::read_appointment(data_dir, vet_id, appointment_id)?
        .ok_or_else(|| AppError::NotFound("appointment not found".to_string()))?;

    if appointment.status != AppointmentStatus::Completed {
        return Err(AppError::Conflict(
            "a visit can only be recorded on a completed appointment".to_string(),
        ));
    }

    if model::find_by_appointment_id(data_dir, appointment_id)?.is_some() {
        return Err(AppError::Conflict(
            "a visit has already been recorded for this appointment".to_string(),
        ));
    }

    let visit = model::Visit {
        id: Uuid::new_v4(),
        appointment_id,
        visit_type: req.visit_type,
        remark: req.remark,
        created_at: OffsetDateTime::now_utc(),
    };
    model::write_visit(data_dir, &visit)?;

    Ok(visit)
}

pub async fn list_for_pet(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(pet_id): PathParam<Uuid>,
) -> Result<Json<Vec<VisitResponse>>, AppError> {
    if auth.role != Role::Owner {
        return Err(AppError::Forbidden(
            "this endpoint requires the owner role".to_string(),
        ));
    }

    let data_dir = config.data_dir.clone();
    let visits =
        tokio::task::spawn_blocking(move || list_for_pet_blocking(&data_dir, auth.id, pet_id))
            .await
            .map_err(|_| AppError::Internal)??;

    Ok(Json(visits.into_iter().map(VisitResponse::from).collect()))
}

fn list_for_pet_blocking(
    data_dir: &Path,
    user_id: Uuid,
    pet_id: Uuid,
) -> Result<Vec<model::Visit>, AppError> {
    let owner_id = resolve_owner_id(data_dir, user_id)?;
    let pet = pets::read_pet(data_dir, pet_id)?.ok_or_else(pet_not_found)?;
    if pet.owner_id != owner_id {
        return Err(pet_not_found());
    }

    let appointment_ids: Vec<Uuid> = appointments::read_all(data_dir)?
        .into_iter()
        .filter(|a| a.pet_id == pet_id)
        .map(|a| a.id)
        .collect();

    Ok(model::find_all_for_appointments(
        data_dir,
        &appointment_ids,
    )?)
}
