use std::path::Path;

use axum::Json;
use axum::extract::{Path as PathParam, State};
use serde::{Deserialize, Serialize};
use time::PrimitiveDateTime;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::config::Config;
use crate::domain::{self, AppointmentStatus, Role};
use crate::error::AppError;
use crate::features::appointments::model as appointments;
use crate::features::pets::model as pets;
use crate::features::registration::model as registration;
use crate::storage;

use super::model;

#[derive(Debug, Deserialize)]
pub struct VetVisitRequest {
    #[serde(rename = "vetSummary", default)]
    pub vet_summary: String,
    #[serde(rename = "ownerSummary", default)]
    pub owner_summary: String,
    #[serde(default)]
    pub vaccination: String,
}

#[derive(Debug, Serialize)]
pub struct VetVisitResponse {
    pub id: i64,
    #[serde(rename = "vetSummary")]
    pub vet_summary: String,
    #[serde(rename = "ownerSummary")]
    pub owner_summary: String,
    pub vaccination: String,
}

impl From<model::Visit> for VetVisitResponse {
    fn from(v: model::Visit) -> Self {
        Self {
            id: domain::wire_id(v.id),
            vet_summary: v.vet_summary,
            owner_summary: v.owner_summary,
            vaccination: v.vaccination,
        }
    }
}

/// `GET /api/owner/pets/{petId}/visits`'s shape — unlike `VetVisitResponse`,
/// no `vetSummary` (the target contract keeps that vet-only), plus the
/// owning vet's username and the appointment's own `startsAt` joined in.
#[derive(Debug, Serialize)]
pub struct OwnerVisitResponse {
    pub id: i64,
    #[serde(rename = "ownerSummary")]
    pub owner_summary: String,
    pub vaccination: String,
    #[serde(rename = "vetUsername")]
    pub vet_username: String,
    #[serde(rename = "startsAt")]
    pub starts_at: PrimitiveDateTime,
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

/// Cross-slice: `OwnerVisitResponse` needs the treating vet's username.
fn resolve_vet_username(data_dir: &Path, vet_id: Uuid) -> Result<String, AppError> {
    let vet = registration::list_all_vets(data_dir)?
        .into_iter()
        .find(|v| v.id == vet_id)
        .ok_or_else(|| AppError::NotFound("vet not found".to_string()))?;
    let user = registration::read_user(data_dir, vet.user_id)?
        .ok_or_else(|| AppError::NotFound("vet not found".to_string()))?;
    Ok(user.username)
}

fn pet_not_found() -> AppError {
    AppError::NotFound("pet not found".to_string())
}

fn appointment_not_found() -> AppError {
    AppError::NotFound("appointment not found".to_string())
}

fn visit_not_found() -> AppError {
    AppError::NotFound("visit not found".to_string())
}

pub async fn get_vet_visit(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(id): PathParam<i64>,
) -> Result<Json<VetVisitResponse>, AppError> {
    if auth.role != Role::Vet {
        return Err(AppError::Forbidden(
            "this endpoint requires the vet role".to_string(),
        ));
    }

    let data_dir = config.data_dir.clone();
    let visit = tokio::task::spawn_blocking(move || get_vet_visit_blocking(&data_dir, auth.id, id))
        .await
        .map_err(|_| AppError::Internal)??;

    Ok(Json(visit.into()))
}

fn get_vet_visit_blocking(
    data_dir: &Path,
    user_id: Uuid,
    wire_id: i64,
) -> Result<model::Visit, AppError> {
    let vet_id = resolve_vet_id(data_dir, user_id)?;
    let appointment =
        appointments::find_by_wire_id(data_dir, wire_id)?.ok_or_else(appointment_not_found)?;
    if appointment.vet_id != vet_id {
        return Err(appointment_not_found());
    }

    model::find_by_appointment_id(data_dir, appointment.id)?.ok_or_else(visit_not_found)
}

pub async fn put_vet_visit(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(id): PathParam<i64>,
    Json(req): Json<VetVisitRequest>,
) -> Result<Json<VetVisitResponse>, AppError> {
    if auth.role != Role::Vet {
        return Err(AppError::Forbidden(
            "this endpoint requires the vet role".to_string(),
        ));
    }

    let data_dir = config.data_dir.clone();
    let visit =
        tokio::task::spawn_blocking(move || put_vet_visit_blocking(&data_dir, auth.id, id, req))
            .await
            .map_err(|_| AppError::Internal)??;

    Ok(Json(visit.into()))
}

/// Creates the visit on first call, replaces it on any later call — `PUT`'s
/// usual upsert semantics, and the target contract's `VetVisitRequest`/
/// `VetVisit` share one URI per appointment rather than this repo's earlier
/// `POST`-then-409-on-a-second-call design. The first call that succeeds
/// also transitions the appointment to `Completed` — the target contract
/// has no separate `complete` endpoint at all, so this is the only path
/// there is now. Reuses `appointments`' own lock and write path (its public
/// `lock_path`/`read_appointment`/`write_appointment`) rather than a second
/// lock scheme, so this stays inside the one `vet-<id>.lock` critical
/// section every other appointment write already goes through
/// (`docs/architecture-internals.md` §1) even though the write originates
/// from a different slice's handler.
fn put_vet_visit_blocking(
    data_dir: &Path,
    user_id: Uuid,
    wire_id: i64,
    req: VetVisitRequest,
) -> Result<model::Visit, AppError> {
    let vet_id = resolve_vet_id(data_dir, user_id)?;
    let located =
        appointments::find_by_wire_id(data_dir, wire_id)?.ok_or_else(appointment_not_found)?;
    if located.vet_id != vet_id {
        return Err(appointment_not_found());
    }

    let _lock = storage::FileLock::exclusive(&appointments::lock_path(data_dir, vet_id))?;

    let mut appointment = appointments::read_appointment(data_dir, vet_id, located.id)?
        .ok_or_else(appointment_not_found)?;

    if appointment
        .status
        .can_transition_to(AppointmentStatus::Completed)
    {
        appointment.status = AppointmentStatus::Completed;
        appointments::write_appointment(data_dir, &appointment)?;
    } else if appointment.status != AppointmentStatus::Completed {
        return Err(AppError::InvalidTransition(format!(
            "cannot record a visit while the appointment is {:?}",
            appointment.status
        )));
    }

    let existing_id = model::find_by_appointment_id(data_dir, appointment.id)?.map(|v| v.id);
    let visit = model::Visit {
        id: existing_id.unwrap_or_else(Uuid::new_v4),
        appointment_id: appointment.id,
        vet_summary: req.vet_summary,
        owner_summary: req.owner_summary,
        vaccination: req.vaccination,
    };
    model::write_visit(data_dir, &visit)?;

    Ok(visit)
}

pub async fn list_owner_pet_visits(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(pet_id): PathParam<i64>,
) -> Result<Json<Vec<OwnerVisitResponse>>, AppError> {
    if auth.role != Role::Owner {
        return Err(AppError::Forbidden(
            "this endpoint requires the owner role".to_string(),
        ));
    }

    let data_dir = config.data_dir.clone();
    let visits = tokio::task::spawn_blocking(move || {
        list_owner_pet_visits_blocking(&data_dir, auth.id, pet_id)
    })
    .await
    .map_err(|_| AppError::Internal)??;

    Ok(Json(visits))
}

fn list_owner_pet_visits_blocking(
    data_dir: &Path,
    user_id: Uuid,
    wire_id: i64,
) -> Result<Vec<OwnerVisitResponse>, AppError> {
    let owner_id = resolve_owner_id(data_dir, user_id)?;
    let pet =
        pets::find_by_owner_and_wire_id(data_dir, owner_id, wire_id)?.ok_or_else(pet_not_found)?;

    let mut responses = Vec::new();
    for appointment in appointments::read_all(data_dir)?
        .into_iter()
        .filter(|a| a.pet_id == pet.id)
    {
        if let Some(visit) = model::find_by_appointment_id(data_dir, appointment.id)? {
            let vet_username = resolve_vet_username(data_dir, appointment.vet_id)?;
            responses.push(OwnerVisitResponse {
                id: domain::wire_id(visit.id),
                owner_summary: visit.owner_summary,
                vaccination: visit.vaccination,
                vet_username,
                starts_at: appointment.time_slot,
            });
        }
    }
    Ok(responses)
}
