use std::path::Path;

use axum::Json;
use axum::extract::{Path as PathParam, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use time::{Date, Duration, OffsetDateTime, PrimitiveDateTime};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::config::Config;
use crate::domain::{AppointmentStatus, Role, TimeRange};
use crate::error::AppError;
use crate::features::admin::activity;
use crate::features::availability::model as availability;
use crate::features::pets::model as pets;
use crate::features::registration::model as registration;

use super::{lock, model, slots};

#[derive(Debug, Deserialize)]
pub struct BookRequest {
    pub pet_id: Uuid,
    pub vet_id: Uuid,
    pub time_slot: PrimitiveDateTime,
    #[serde(default)]
    pub duration_minutes: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct RescheduleRequest {
    pub time_slot: PrimitiveDateTime,
    #[serde(default)]
    pub duration_minutes: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct FreeSlotsQuery {
    pub date: Date,
}

#[derive(Debug, Serialize)]
pub struct AppointmentResponse {
    pub id: Uuid,
    pub pet_id: Uuid,
    pub vet_id: Uuid,
    pub time_slot: PrimitiveDateTime,
    pub duration_minutes: i64,
    pub status: AppointmentStatus,
}

impl From<model::Appointment> for AppointmentResponse {
    fn from(a: model::Appointment) -> Self {
        Self {
            id: a.id,
            pet_id: a.pet_id,
            vet_id: a.vet_id,
            time_slot: a.time_slot,
            duration_minutes: a.duration_minutes,
            status: a.status,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct TimeRangeResponse {
    pub start: PrimitiveDateTime,
    pub end: PrimitiveDateTime,
}

impl From<TimeRange> for TimeRangeResponse {
    fn from(r: TimeRange) -> Self {
        Self {
            start: r.start,
            end: r.end,
        }
    }
}

fn resolve_owner_id(data_dir: &Path, user_id: Uuid) -> Result<Uuid, AppError> {
    let owner = registration::find_owner_by_user_id(data_dir, user_id)?
        .ok_or_else(|| AppError::Forbidden("no owner profile for this account".to_string()))?;
    Ok(owner.id)
}

fn resolve_vet_id(data_dir: &Path, user_id: Uuid) -> Result<Uuid, AppError> {
    let vet = registration::find_vet_by_user_id(data_dir, user_id)?
        .ok_or_else(|| AppError::Forbidden("no vet profile for this account".to_string()))?;
    Ok(vet.id)
}

fn verify_pet_owned_by(data_dir: &Path, pet_id: Uuid, owner_id: Uuid) -> Result<(), AppError> {
    let pet = pets::read_pet(data_dir, pet_id)?.ok_or_else(pet_not_found)?;
    if pet.owner_id != owner_id {
        return Err(pet_not_found());
    }
    Ok(())
}

fn pet_not_found() -> AppError {
    AppError::NotFound("pet not found".to_string())
}

fn appointment_not_found() -> AppError {
    AppError::NotFound("appointment not found".to_string())
}

/// Best-effort: a logging failure shouldn't fail an
/// appointment mutation that already succeeded.
fn log_activity(data_dir: &Path, event_type: &str, details: serde_json::Value) {
    if let Err(e) = activity::record(data_dir, event_type, details) {
        tracing::warn!(error = %e, "failed to record activity log entry");
    }
}

fn free_slots_for(
    data_dir: &Path,
    vet_id: Uuid,
    date: Date,
    exclude_appointment_id: Option<Uuid>,
) -> Result<Vec<TimeRange>, AppError> {
    let weekly = availability::read_weekly(data_dir, vet_id)?;
    let exception = availability::find_exception_by_date(data_dir, vet_id, date)?;
    let active: Vec<_> = model::read_active_for_vet(data_dir, vet_id)?
        .into_iter()
        .filter(|a| Some(a.id) != exclude_appointment_id)
        .collect();

    Ok(slots::derive_free_slots(
        date,
        &weekly,
        exception.as_ref(),
        &active,
    ))
}

pub async fn get_slots(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(vet_id): PathParam<Uuid>,
    Query(query): Query<FreeSlotsQuery>,
) -> Result<Json<Vec<TimeRangeResponse>>, AppError> {
    if auth.role != Role::Owner {
        return Err(AppError::Forbidden(
            "this endpoint requires the owner role".to_string(),
        ));
    }

    let data_dir = config.data_dir.clone();
    let free =
        tokio::task::spawn_blocking(move || get_slots_blocking(&data_dir, vet_id, query.date))
            .await
            .map_err(|_| AppError::Internal)??;

    Ok(Json(
        free.into_iter().map(TimeRangeResponse::from).collect(),
    ))
}

/// Read path: a shared lock is enough — it guards against observing this
/// vet's directory mid-way through a concurrent write, which the write
/// path's atomic rename doesn't cover for a *multi-file* view (`docs/architecture-internals.md` §1).
fn get_slots_blocking(
    data_dir: &Path,
    vet_id: Uuid,
    date: Date,
) -> Result<Vec<TimeRange>, AppError> {
    let _lock = crate::storage::FileLock::shared(&model::lock_path(data_dir, vet_id))?;
    free_slots_for(data_dir, vet_id, date, None)
}

pub async fn book(
    State(config): State<Config>,
    auth: AuthUser,
    Json(req): Json<BookRequest>,
) -> Result<(StatusCode, Json<AppointmentResponse>), AppError> {
    if auth.role != Role::Owner {
        return Err(AppError::Forbidden(
            "only owners can book appointments".to_string(),
        ));
    }

    let duration = req
        .duration_minutes
        .unwrap_or(config.appointment_default_duration_min);
    if duration <= 0 {
        return Err(AppError::Validation(
            "duration_minutes must be positive".to_string(),
        ));
    }

    let data_dir = config.data_dir.clone();
    let (pet_id, vet_id, time_slot) = (req.pet_id, req.vet_id, req.time_slot);
    let appointment = tokio::task::spawn_blocking(move || {
        book_blocking(&data_dir, auth.id, pet_id, vet_id, time_slot, duration)
    })
    .await
    .map_err(|_| AppError::Internal)??;

    Ok((StatusCode::CREATED, Json(appointment.into())))
}

/// The critical-section write path (`docs/architecture-internals.md` §1): read-check-then-write, all
/// under one exclusive vet lock.
fn book_blocking(
    data_dir: &Path,
    user_id: Uuid,
    pet_id: Uuid,
    vet_id: Uuid,
    time_slot: PrimitiveDateTime,
    duration: i64,
) -> Result<model::Appointment, AppError> {
    let owner_id = resolve_owner_id(data_dir, user_id)?;
    verify_pet_owned_by(data_dir, pet_id, owner_id)?;

    let _lock = lock::acquire(data_dir, vet_id)?;

    let free = free_slots_for(data_dir, vet_id, time_slot.date(), None)?;
    let requested = TimeRange::new(time_slot, time_slot + Duration::minutes(duration));
    if !slots::fits_within_free_slots(&free, &requested) {
        return Err(AppError::SlotUnavailable);
    }

    let appointment = model::Appointment {
        id: Uuid::new_v4(),
        pet_id,
        vet_id,
        time_slot,
        duration_minutes: duration,
        status: AppointmentStatus::Booked,
    };
    model::write_appointment(data_dir, &appointment)?;
    log_activity(
        data_dir,
        "appointment_booked",
        serde_json::json!({"appointment_id": appointment.id, "vet_id": vet_id, "pet_id": pet_id}),
    );

    Ok(appointment)
}

/// Resolves which vet's directory holds `appointment_id` and checks the
/// caller may act on it: a vet only on their own appointments, an owner
/// only on an appointment for one of their own pets.
fn locate_and_authorize(
    data_dir: &Path,
    role: Role,
    user_id: Uuid,
    appointment_id: Uuid,
) -> Result<Uuid, AppError> {
    match role {
        Role::Vet => {
            let vet_id = resolve_vet_id(data_dir, user_id)?;
            if model::read_appointment(data_dir, vet_id, appointment_id)?.is_none() {
                return Err(appointment_not_found());
            }
            Ok(vet_id)
        }
        Role::Owner => {
            let owner_id = resolve_owner_id(data_dir, user_id)?;
            let vet_id = model::find_vet_id_for_appointment(data_dir, appointment_id)?
                .ok_or_else(appointment_not_found)?;
            let appointment = model::read_appointment(data_dir, vet_id, appointment_id)?
                .ok_or_else(appointment_not_found)?;
            verify_pet_owned_by(data_dir, appointment.pet_id, owner_id)?;
            Ok(vet_id)
        }
        Role::Admin => Err(AppError::Forbidden(
            "this endpoint requires the owner or vet role".to_string(),
        )),
    }
}

pub async fn cancel(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(id): PathParam<Uuid>,
) -> Result<Json<AppointmentResponse>, AppError> {
    if auth.role != Role::Owner && auth.role != Role::Vet {
        return Err(AppError::Forbidden(
            "this endpoint requires the owner or vet role".to_string(),
        ));
    }

    let data_dir = config.data_dir.clone();
    let cutoff_hours = config.cancellation_cutoff_hours;
    let appointment = tokio::task::spawn_blocking(move || {
        cancel_blocking(&data_dir, auth.role, auth.id, id, cutoff_hours)
    })
    .await
    .map_err(|_| AppError::Internal)??;

    Ok(Json(appointment.into()))
}

fn cancel_blocking(
    data_dir: &Path,
    role: Role,
    user_id: Uuid,
    appointment_id: Uuid,
    cutoff_hours: i64,
) -> Result<model::Appointment, AppError> {
    let vet_id = locate_and_authorize(data_dir, role, user_id, appointment_id)?;
    let _lock = lock::acquire(data_dir, vet_id)?;

    let mut appointment = model::read_appointment(data_dir, vet_id, appointment_id)?
        .ok_or_else(appointment_not_found)?;

    if !appointment
        .status
        .can_transition_to(AppointmentStatus::Cancelled)
    {
        return Err(AppError::InvalidTransition(format!(
            "cannot cancel an appointment that is {:?}",
            appointment.status
        )));
    }

    let now = now_naive();
    if appointment.time_slot - now < Duration::hours(cutoff_hours) {
        return Err(AppError::CancellationCutoffPassed);
    }

    appointment.status = AppointmentStatus::Cancelled;
    model::write_appointment(data_dir, &appointment)?;
    log_activity(
        data_dir,
        "appointment_cancelled",
        serde_json::json!({"appointment_id": appointment.id, "vet_id": vet_id}),
    );

    Ok(appointment)
}

/// This repo doesn't model per-location timezones — "now"
/// is treated as naive UTC, matched directly against the naive `time_slot`
/// appointments are stored with.
fn now_naive() -> PrimitiveDateTime {
    let now = OffsetDateTime::now_utc();
    PrimitiveDateTime::new(now.date(), now.time())
}

pub async fn reschedule(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(id): PathParam<Uuid>,
    Json(req): Json<RescheduleRequest>,
) -> Result<Json<AppointmentResponse>, AppError> {
    if auth.role != Role::Owner {
        return Err(AppError::Forbidden(
            "this endpoint requires the owner role".to_string(),
        ));
    }

    let duration = req
        .duration_minutes
        .unwrap_or(config.appointment_default_duration_min);
    if duration <= 0 {
        return Err(AppError::Validation(
            "duration_minutes must be positive".to_string(),
        ));
    }

    let data_dir = config.data_dir.clone();
    let time_slot = req.time_slot;
    let appointment = tokio::task::spawn_blocking(move || {
        reschedule_blocking(&data_dir, auth.id, id, time_slot, duration)
    })
    .await
    .map_err(|_| AppError::Internal)??;

    Ok(Json(appointment.into()))
}

/// Cancel-old + book-new inside one lock acquisition (`docs/architecture-internals.md` §3) — not two
/// separate critical sections, so nothing else can slot into the old
/// appointment's freed time between the two writes.
fn reschedule_blocking(
    data_dir: &Path,
    user_id: Uuid,
    appointment_id: Uuid,
    new_time_slot: PrimitiveDateTime,
    duration: i64,
) -> Result<model::Appointment, AppError> {
    let owner_id = resolve_owner_id(data_dir, user_id)?;
    let vet_id = model::find_vet_id_for_appointment(data_dir, appointment_id)?
        .ok_or_else(appointment_not_found)?;

    let _lock = lock::acquire(data_dir, vet_id)?;

    let mut old = model::read_appointment(data_dir, vet_id, appointment_id)?
        .ok_or_else(appointment_not_found)?;
    verify_pet_owned_by(data_dir, old.pet_id, owner_id)?;

    if !old.status.can_transition_to(AppointmentStatus::Cancelled) {
        return Err(AppError::InvalidTransition(format!(
            "cannot reschedule an appointment that is {:?}",
            old.status
        )));
    }

    // Exclude the appointment being rescheduled from the busy set — it
    // currently occupies time that would otherwise block its own new slot.
    let free = free_slots_for(data_dir, vet_id, new_time_slot.date(), Some(appointment_id))?;
    let requested = TimeRange::new(new_time_slot, new_time_slot + Duration::minutes(duration));
    if !slots::fits_within_free_slots(&free, &requested) {
        return Err(AppError::SlotUnavailable);
    }

    old.status = AppointmentStatus::Cancelled;
    model::write_appointment(data_dir, &old)?;

    let new_appointment = model::Appointment {
        id: Uuid::new_v4(),
        pet_id: old.pet_id,
        vet_id,
        time_slot: new_time_slot,
        duration_minutes: duration,
        status: AppointmentStatus::Booked,
    };
    model::write_appointment(data_dir, &new_appointment)?;
    log_activity(
        data_dir,
        "appointment_rescheduled",
        serde_json::json!({
            "old_appointment_id": appointment_id,
            "new_appointment_id": new_appointment.id,
            "vet_id": vet_id,
        }),
    );

    Ok(new_appointment)
}

pub async fn confirm(
    state: State<Config>,
    auth: AuthUser,
    path: PathParam<Uuid>,
) -> Result<Json<AppointmentResponse>, AppError> {
    vet_transition(state, auth, path, AppointmentStatus::Confirmed).await
}

pub async fn complete(
    state: State<Config>,
    auth: AuthUser,
    path: PathParam<Uuid>,
) -> Result<Json<AppointmentResponse>, AppError> {
    vet_transition(state, auth, path, AppointmentStatus::Completed).await
}

pub async fn no_show(
    state: State<Config>,
    auth: AuthUser,
    path: PathParam<Uuid>,
) -> Result<Json<AppointmentResponse>, AppError> {
    vet_transition(state, auth, path, AppointmentStatus::NoShow).await
}

async fn vet_transition(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(id): PathParam<Uuid>,
    next: AppointmentStatus,
) -> Result<Json<AppointmentResponse>, AppError> {
    if auth.role != Role::Vet {
        return Err(AppError::Forbidden(
            "this endpoint requires the vet role".to_string(),
        ));
    }

    let data_dir = config.data_dir.clone();
    let appointment =
        tokio::task::spawn_blocking(move || vet_transition_blocking(&data_dir, auth.id, id, next))
            .await
            .map_err(|_| AppError::Internal)??;

    Ok(Json(appointment.into()))
}

fn vet_transition_blocking(
    data_dir: &Path,
    user_id: Uuid,
    appointment_id: Uuid,
    next: AppointmentStatus,
) -> Result<model::Appointment, AppError> {
    let vet_id = resolve_vet_id(data_dir, user_id)?;
    let _lock = lock::acquire(data_dir, vet_id)?;

    let mut appointment = model::read_appointment(data_dir, vet_id, appointment_id)?
        .ok_or_else(appointment_not_found)?;

    if !appointment.status.can_transition_to(next) {
        return Err(AppError::InvalidTransition(format!(
            "cannot move {:?} to {next:?}",
            appointment.status
        )));
    }

    appointment.status = next;
    model::write_appointment(data_dir, &appointment)?;
    let event_type = match next {
        AppointmentStatus::Confirmed => "appointment_confirmed",
        AppointmentStatus::Completed => "appointment_completed",
        AppointmentStatus::NoShow => "appointment_no_show",
        // `can_transition_to` above already rejects any other target.
        _ => "appointment_status_changed",
    };
    log_activity(
        data_dir,
        event_type,
        serde_json::json!({"appointment_id": appointment.id, "vet_id": vet_id}),
    );

    Ok(appointment)
}

pub async fn list_mine(
    State(config): State<Config>,
    auth: AuthUser,
) -> Result<Json<Vec<AppointmentResponse>>, AppError> {
    let data_dir = config.data_dir.clone();
    let appointments =
        tokio::task::spawn_blocking(move || list_mine_blocking(&data_dir, auth.role, auth.id))
            .await
            .map_err(|_| AppError::Internal)??;

    Ok(Json(
        appointments
            .into_iter()
            .map(AppointmentResponse::from)
            .collect(),
    ))
}

fn list_mine_blocking(
    data_dir: &Path,
    role: Role,
    user_id: Uuid,
) -> Result<Vec<model::Appointment>, AppError> {
    match role {
        Role::Vet => {
            let vet_id = resolve_vet_id(data_dir, user_id)?;
            // Same read-path rationale as `get_slots_blocking`.
            let _lock = crate::storage::FileLock::shared(&model::lock_path(data_dir, vet_id))?;
            Ok(model::read_all_for_vet(data_dir, vet_id)?)
        }
        Role::Owner => {
            let owner_id = resolve_owner_id(data_dir, user_id)?;
            let pet_ids: std::collections::HashSet<Uuid> =
                pets::list_pets_for_owner(data_dir, owner_id)?
                    .into_iter()
                    .map(|p| p.id)
                    .collect();
            Ok(model::read_all(data_dir)?
                .into_iter()
                .filter(|a| pet_ids.contains(&a.pet_id))
                .collect())
        }
        Role::Admin => Err(AppError::Forbidden(
            "this endpoint requires the owner or vet role".to_string(),
        )),
    }
}
