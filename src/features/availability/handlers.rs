use std::path::Path;

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use time::{Date, Time};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::config::Config;
use crate::domain::Role;
use crate::error::AppError;
use crate::features::registration::model as registration;
use crate::storage;

use super::model::{self, DayOfWeek};

#[derive(Debug, Deserialize)]
pub struct SlotInput {
    pub day_of_week: DayOfWeek,
    pub start_time: Time,
    pub end_time: Time,
}

#[derive(Debug, Deserialize)]
pub struct SetWeeklyAvailabilityRequest {
    pub slots: Vec<SlotInput>,
}

#[derive(Debug, Serialize)]
pub struct AvailabilitySlotResponse {
    pub id: Uuid,
    pub day_of_week: DayOfWeek,
    pub start_time: Time,
    pub end_time: Time,
}

#[derive(Debug, Deserialize)]
pub struct SetAvailabilityExceptionRequest {
    pub date: Date,
    pub is_available: bool,
    #[serde(default)]
    pub start_time: Option<Time>,
    #[serde(default)]
    pub end_time: Option<Time>,
}

#[derive(Debug, Serialize)]
pub struct AvailabilityExceptionResponse {
    pub id: Uuid,
    pub date: Date,
    pub is_available: bool,
    pub start_time: Option<Time>,
    pub end_time: Option<Time>,
}

fn require_vet(auth: &AuthUser) -> Result<(), AppError> {
    if auth.role != Role::Vet {
        return Err(AppError::Forbidden(
            "this endpoint requires the vet role".to_string(),
        ));
    }
    Ok(())
}

fn resolve_vet_id(data_dir: &Path, user_id: Uuid) -> Result<Uuid, AppError> {
    let vet = registration::find_vet_by_user_id(data_dir, user_id)?
        .ok_or_else(|| AppError::Forbidden("no vet profile for this account".to_string()))?;
    Ok(vet.id)
}

fn validate_slots(slots: &[SlotInput]) -> Result<(), AppError> {
    for slot in slots {
        if slot.start_time >= slot.end_time {
            return Err(AppError::Validation(
                "start_time must be before end_time".to_string(),
            ));
        }
    }

    for i in 0..slots.len() {
        for j in (i + 1)..slots.len() {
            let (a, b) = (&slots[i], &slots[j]);
            if a.day_of_week == b.day_of_week
                && a.start_time < b.end_time
                && b.start_time < a.end_time
            {
                return Err(AppError::Validation(format!(
                    "overlapping slots on {:?}",
                    a.day_of_week
                )));
            }
        }
    }

    Ok(())
}

fn validate_exception(req: &SetAvailabilityExceptionRequest) -> Result<(), AppError> {
    if req.is_available {
        match (req.start_time, req.end_time) {
            (Some(start), Some(end)) if start < end => Ok(()),
            (Some(_), Some(_)) => Err(AppError::Validation(
                "start_time must be before end_time".to_string(),
            )),
            _ => Err(AppError::Validation(
                "start_time and end_time are required when is_available is true".to_string(),
            )),
        }
    } else if req.start_time.is_some() || req.end_time.is_some() {
        Err(AppError::Validation(
            "start_time/end_time must be omitted when is_available is false".to_string(),
        ))
    } else {
        Ok(())
    }
}

pub async fn set_weekly(
    State(config): State<Config>,
    auth: AuthUser,
    Json(req): Json<SetWeeklyAvailabilityRequest>,
) -> Result<Json<Vec<AvailabilitySlotResponse>>, AppError> {
    require_vet(&auth)?;
    validate_slots(&req.slots)?;

    let data_dir = config.data_dir.clone();
    let response =
        tokio::task::spawn_blocking(move || set_weekly_blocking(&data_dir, auth.id, req))
            .await
            .map_err(|_| AppError::Internal)??;

    Ok(Json(response))
}

/// Deletes then rewrites the whole weekly schedule under one lock
/// acquisition, so a concurrent reader (e.g. `appointments` deriving free
/// slots) never observes the directory between "old slots gone" and "new
/// slots written" (PLAN.md §5).
fn set_weekly_blocking(
    data_dir: &Path,
    user_id: Uuid,
    req: SetWeeklyAvailabilityRequest,
) -> Result<Vec<AvailabilitySlotResponse>, AppError> {
    let vet_id = resolve_vet_id(data_dir, user_id)?;
    let _lock = storage::FileLock::exclusive(&model::lock_path(data_dir, vet_id))?;

    model::delete_all_slots(data_dir, vet_id)?;

    req.slots
        .into_iter()
        .map(|input| {
            let slot = model::AvailabilitySlot {
                id: Uuid::new_v4(),
                vet_id,
                day_of_week: input.day_of_week,
                start_time: input.start_time,
                end_time: input.end_time,
            };
            model::write_slot(data_dir, &slot)?;
            Ok(AvailabilitySlotResponse {
                id: slot.id,
                day_of_week: slot.day_of_week,
                start_time: slot.start_time,
                end_time: slot.end_time,
            })
        })
        .collect()
}

pub async fn set_exception(
    State(config): State<Config>,
    auth: AuthUser,
    Json(req): Json<SetAvailabilityExceptionRequest>,
) -> Result<Json<AvailabilityExceptionResponse>, AppError> {
    require_vet(&auth)?;
    validate_exception(&req)?;

    let data_dir = config.data_dir.clone();
    let response =
        tokio::task::spawn_blocking(move || set_exception_blocking(&data_dir, auth.id, req))
            .await
            .map_err(|_| AppError::Internal)??;

    Ok(Json(response))
}

/// At most one exception per vet per date: reuses the existing record's id
/// (if any) so the write lands on the same path and replaces it, rather
/// than accumulating one file per call for the same date.
fn set_exception_blocking(
    data_dir: &Path,
    user_id: Uuid,
    req: SetAvailabilityExceptionRequest,
) -> Result<AvailabilityExceptionResponse, AppError> {
    let vet_id = resolve_vet_id(data_dir, user_id)?;
    let _lock = storage::FileLock::exclusive(&model::lock_path(data_dir, vet_id))?;

    let id = model::find_exception_by_date(data_dir, vet_id, req.date)?
        .map(|existing| existing.id)
        .unwrap_or_else(Uuid::new_v4);

    let exception = model::AvailabilityException {
        id,
        vet_id,
        date: req.date,
        is_available: req.is_available,
        start_time: req.start_time,
        end_time: req.end_time,
    };
    model::write_exception(data_dir, &exception)?;

    Ok(AvailabilityExceptionResponse {
        id: exception.id,
        date: exception.date,
        is_available: exception.is_available,
        start_time: exception.start_time,
        end_time: exception.end_time,
    })
}
