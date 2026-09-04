use std::path::Path;

use axum::Json;
use axum::extract::{Path as PathParam, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use time::{Date, Duration, OffsetDateTime, PrimitiveDateTime};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::config::Config;
use crate::domain::{self, AppointmentStatus, AppointmentType, Role, TimeRange};
use crate::error::AppError;
use crate::features::admin::activity;
use crate::features::locations::model as locations;
use crate::features::locations::slots as locations_slots;
use crate::features::pets::model as pets;
use crate::features::registration::model as registration;

use super::{lock, model};

/// The target contract's `AppointmentRequest` — no duration (always
/// `Config::appointment_default_duration_min`, since nothing in the domain
/// model ties `appointmentType` to a different length) and no `vetId` (the
/// vet is resolved from `locationId`, since a location belongs to exactly
/// one vet).
#[derive(Debug, Deserialize)]
pub struct AppointmentRequest {
    #[serde(rename = "locationId")]
    pub location_id: i64,
    #[serde(rename = "petId")]
    pub pet_id: i64,
    #[serde(rename = "startsAt")]
    pub starts_at: PrimitiveDateTime,
    #[serde(rename = "appointmentType")]
    pub appointment_type: AppointmentType,
}

#[derive(Debug, Deserialize)]
pub struct RescheduleRequest {
    #[serde(rename = "startsAt")]
    pub starts_at: PrimitiveDateTime,
}

#[derive(Debug, Serialize)]
pub struct AppointmentResponse {
    pub id: i64,
    #[serde(rename = "vetId")]
    pub vet_id: i64,
    #[serde(rename = "petId")]
    pub pet_id: i64,
    #[serde(rename = "startsAt")]
    pub starts_at: PrimitiveDateTime,
    #[serde(rename = "locationId")]
    pub location_id: i64,
    #[serde(rename = "endsAt")]
    pub ends_at: PrimitiveDateTime,
    pub status: AppointmentStatus,
    #[serde(rename = "appointmentType")]
    pub appointment_type: AppointmentType,
}

impl From<model::Appointment> for AppointmentResponse {
    fn from(a: model::Appointment) -> Self {
        Self {
            id: domain::wire_id(a.id),
            vet_id: domain::wire_id(a.vet_id),
            pet_id: domain::wire_id(a.pet_id),
            starts_at: a.time_slot,
            location_id: domain::wire_id(a.location_id),
            ends_at: a.time_slot + Duration::minutes(a.duration_minutes),
            status: a.status,
            appointment_type: a.appointment_type,
        }
    }
}

/// `GET /api/vet/appointments`'s shape — a denormalized view (pet name,
/// owner username) a vet's calendar needs and `AppointmentResponse` doesn't
/// carry, matching the target contract's `VetAppointment` exactly (no
/// `locationId`/`endsAt` here, unlike `AppointmentResponse`).
#[derive(Debug, Serialize)]
pub struct VetAppointmentResponse {
    pub id: i64,
    #[serde(rename = "petId")]
    pub pet_id: i64,
    #[serde(rename = "petName")]
    pub pet_name: String,
    #[serde(rename = "ownerUsername")]
    pub owner_username: String,
    #[serde(rename = "startsAt")]
    pub starts_at: PrimitiveDateTime,
    pub status: AppointmentStatus,
    #[serde(rename = "appointmentType")]
    pub appointment_type: AppointmentType,
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

/// Cross-slice: `VetAppointmentResponse` needs the owner's username, and
/// only has the `Owner` id a pet is keyed by.
fn resolve_owner_username(data_dir: &Path, owner_id: Uuid) -> Result<String, AppError> {
    let owner = registration::list_all_owners(data_dir)?
        .into_iter()
        .find(|o| o.id == owner_id)
        .ok_or_else(|| AppError::NotFound("owner not found".to_string()))?;
    let user = registration::read_user(data_dir, owner.user_id)?
        .ok_or_else(|| AppError::NotFound("owner not found".to_string()))?;
    Ok(user.username)
}

fn pet_not_found() -> AppError {
    AppError::NotFound("pet not found".to_string())
}

fn location_not_found() -> AppError {
    AppError::NotFound("location not found".to_string())
}

fn appointment_not_found() -> AppError {
    AppError::NotFound("appointment not found".to_string())
}

/// Best-effort: a missing username shouldn't fail an appointment mutation
/// that already succeeded — falls back to an empty string, same as the
/// logging failure below it.
fn resolve_username(data_dir: &Path, user_id: Uuid) -> String {
    registration::read_user(data_dir, user_id)
        .ok()
        .flatten()
        .map(|u| u.username)
        .unwrap_or_default()
}

/// Best-effort: a logging failure shouldn't fail an
/// appointment mutation that already succeeded.
fn log_activity(data_dir: &Path, username: &str, event_type: &str, details: serde_json::Value) {
    if let Err(e) = activity::record(data_dir, username, event_type, details) {
        tracing::warn!(error = %e, "failed to record activity log entry");
    }
}

/// The free `[start, end)` windows for `location` on `date` — its own
/// weekly periods/override, minus its vet's *entire* active-appointment
/// calendar (not just appointments at this one location — see
/// `locations::slots`'s doc comment for why that's scoped to the vet).
fn free_slots_for(
    data_dir: &Path,
    location: &locations::Location,
    date: Date,
    exclude_appointment_id: Option<Uuid>,
) -> Result<Vec<TimeRange>, AppError> {
    let weekly = locations::read_periods(data_dir, location.id)?;
    let over = locations::find_override_by_date(data_dir, location.id, date)?;
    let busy: Vec<TimeRange> = model::read_active_for_vet(data_dir, location.vet_id)?
        .into_iter()
        .filter(|a| Some(a.id) != exclude_appointment_id)
        .map(|a| a.time_range())
        .collect();

    Ok(locations_slots::derive_free_slots(
        date,
        &weekly,
        over.as_ref(),
        &busy,
    ))
}

pub async fn create_appointment(
    State(config): State<Config>,
    auth: AuthUser,
    Json(req): Json<AppointmentRequest>,
) -> Result<(StatusCode, Json<AppointmentResponse>), AppError> {
    if auth.role != Role::Owner {
        return Err(AppError::Forbidden(
            "only owners can book appointments".to_string(),
        ));
    }

    let data_dir = config.data_dir.clone();
    let duration = config.appointment_default_duration_min;
    let appointment = tokio::task::spawn_blocking(move || {
        create_appointment_blocking(&data_dir, auth.id, req, duration)
    })
    .await
    .map_err(|_| AppError::Internal)??;

    Ok((StatusCode::CREATED, Json(appointment.into())))
}

/// The critical-section write path (`docs/architecture-internals.md` §1):
/// read-check-then-write, all under one exclusive vet lock.
fn create_appointment_blocking(
    data_dir: &Path,
    user_id: Uuid,
    req: AppointmentRequest,
    duration: i64,
) -> Result<model::Appointment, AppError> {
    let owner_id = resolve_owner_id(data_dir, user_id)?;
    let pet = pets::find_by_owner_and_wire_id(data_dir, owner_id, req.pet_id)?
        .ok_or_else(pet_not_found)?;
    let location =
        locations::find_by_wire_id(data_dir, req.location_id)?.ok_or_else(location_not_found)?;

    let _lock = lock::acquire(data_dir, location.vet_id)?;

    let free = free_slots_for(data_dir, &location, req.starts_at.date(), None)?;
    let requested = TimeRange::new(req.starts_at, req.starts_at + Duration::minutes(duration));
    if !locations_slots::fits_within_free_slots(&free, &requested) {
        return Err(AppError::SlotUnavailable);
    }

    let appointment = model::Appointment {
        id: Uuid::new_v4(),
        pet_id: pet.id,
        vet_id: location.vet_id,
        location_id: location.id,
        time_slot: req.starts_at,
        duration_minutes: duration,
        status: AppointmentStatus::Booked,
        appointment_type: req.appointment_type,
    };
    model::write_appointment(data_dir, &appointment)?;
    log_activity(
        data_dir,
        &resolve_username(data_dir, user_id),
        "appointment_booked",
        serde_json::json!({
            "appointment_id": appointment.id,
            "vet_id": location.vet_id,
            "pet_id": pet.id,
        }),
    );

    Ok(appointment)
}

/// Resolves the appointment a wire id names and checks the caller may act
/// on it: a vet only on their own appointments, an owner only on an
/// appointment for one of their own pets.
fn locate_and_authorize(
    data_dir: &Path,
    role: Role,
    user_id: Uuid,
    wire_id: i64,
) -> Result<model::Appointment, AppError> {
    let appointment =
        model::find_by_wire_id(data_dir, wire_id)?.ok_or_else(appointment_not_found)?;

    match role {
        Role::Vet => {
            let vet_id = resolve_vet_id(data_dir, user_id)?;
            if appointment.vet_id != vet_id {
                return Err(appointment_not_found());
            }
        }
        Role::Owner => {
            let owner_id = resolve_owner_id(data_dir, user_id)?;
            let pet =
                pets::read_pet(data_dir, appointment.pet_id)?.ok_or_else(appointment_not_found)?;
            if pet.owner_id != owner_id {
                return Err(appointment_not_found());
            }
        }
        Role::Admin => {
            return Err(AppError::Forbidden(
                "this endpoint requires the owner or vet role".to_string(),
            ));
        }
    }

    Ok(appointment)
}

pub async fn cancel_owner_appointment(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(id): PathParam<i64>,
) -> Result<StatusCode, AppError> {
    if auth.role != Role::Owner {
        return Err(AppError::Forbidden(
            "this endpoint requires the owner role".to_string(),
        ));
    }
    cancel(config, Role::Owner, auth.id, id).await
}

pub async fn cancel_vet_appointment(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(id): PathParam<i64>,
) -> Result<StatusCode, AppError> {
    if auth.role != Role::Vet {
        return Err(AppError::Forbidden(
            "this endpoint requires the vet role".to_string(),
        ));
    }
    cancel(config, Role::Vet, auth.id, id).await
}

async fn cancel(
    config: Config,
    role: Role,
    user_id: Uuid,
    id: i64,
) -> Result<StatusCode, AppError> {
    let data_dir = config.data_dir.clone();
    let cutoff_hours = config.cancellation_cutoff_hours;
    tokio::task::spawn_blocking(move || {
        cancel_blocking(&data_dir, role, user_id, id, cutoff_hours)
    })
    .await
    .map_err(|_| AppError::Internal)??;

    Ok(StatusCode::OK)
}

fn cancel_blocking(
    data_dir: &Path,
    role: Role,
    user_id: Uuid,
    wire_id: i64,
    cutoff_hours: i64,
) -> Result<(), AppError> {
    let located = locate_and_authorize(data_dir, role, user_id, wire_id)?;
    let _lock = lock::acquire(data_dir, located.vet_id)?;

    let mut appointment = model::read_appointment(data_dir, located.vet_id, located.id)?
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
        &resolve_username(data_dir, user_id),
        "appointment_cancelled",
        serde_json::json!({"appointment_id": appointment.id, "vet_id": appointment.vet_id}),
    );

    Ok(())
}

/// This repo doesn't model per-location timezones — "now"
/// is treated as naive UTC, matched directly against the naive `time_slot`
/// appointments are stored with.
fn now_naive() -> PrimitiveDateTime {
    let now = OffsetDateTime::now_utc();
    PrimitiveDateTime::new(now.date(), now.time())
}

pub async fn reschedule_appointment(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(id): PathParam<i64>,
    Json(req): Json<RescheduleRequest>,
) -> Result<Json<AppointmentResponse>, AppError> {
    if auth.role != Role::Owner {
        return Err(AppError::Forbidden(
            "this endpoint requires the owner role".to_string(),
        ));
    }

    let data_dir = config.data_dir.clone();
    let duration = config.appointment_default_duration_min;
    let starts_at = req.starts_at;
    let appointment = tokio::task::spawn_blocking(move || {
        reschedule_blocking(&data_dir, auth.id, id, starts_at, duration)
    })
    .await
    .map_err(|_| AppError::Internal)??;

    Ok(Json(appointment.into()))
}

/// Cancel-old + book-new inside one lock acquisition
/// (`docs/architecture-internals.md` §3) — not two separate critical
/// sections, so nothing else can slot into the old appointment's freed time
/// between the two writes. The location doesn't change on a reschedule —
/// the target contract's `RescheduleRequest` carries only `startsAt`.
fn reschedule_blocking(
    data_dir: &Path,
    user_id: Uuid,
    wire_id: i64,
    new_starts_at: PrimitiveDateTime,
    duration: i64,
) -> Result<model::Appointment, AppError> {
    let owner_id = resolve_owner_id(data_dir, user_id)?;
    let located = model::find_by_wire_id(data_dir, wire_id)?.ok_or_else(appointment_not_found)?;
    let pet = pets::read_pet(data_dir, located.pet_id)?.ok_or_else(appointment_not_found)?;
    if pet.owner_id != owner_id {
        return Err(appointment_not_found());
    }

    let _lock = lock::acquire(data_dir, located.vet_id)?;

    let mut old = model::read_appointment(data_dir, located.vet_id, located.id)?
        .ok_or_else(appointment_not_found)?;

    if !old.status.can_transition_to(AppointmentStatus::Cancelled) {
        return Err(AppError::InvalidTransition(format!(
            "cannot reschedule an appointment that is {:?}",
            old.status
        )));
    }

    let location =
        locations::read_location(data_dir, old.location_id)?.ok_or_else(location_not_found)?;

    // Exclude the appointment being rescheduled from the busy set — it
    // currently occupies time that would otherwise block its own new slot.
    let free = free_slots_for(data_dir, &location, new_starts_at.date(), Some(old.id))?;
    let requested = TimeRange::new(new_starts_at, new_starts_at + Duration::minutes(duration));
    if !locations_slots::fits_within_free_slots(&free, &requested) {
        return Err(AppError::SlotUnavailable);
    }

    old.status = AppointmentStatus::Cancelled;
    model::write_appointment(data_dir, &old)?;

    let new_appointment = model::Appointment {
        id: Uuid::new_v4(),
        pet_id: old.pet_id,
        vet_id: old.vet_id,
        location_id: old.location_id,
        time_slot: new_starts_at,
        duration_minutes: duration,
        status: AppointmentStatus::Booked,
        appointment_type: old.appointment_type,
    };
    model::write_appointment(data_dir, &new_appointment)?;
    log_activity(
        data_dir,
        &resolve_username(data_dir, user_id),
        "appointment_rescheduled",
        serde_json::json!({
            "old_appointment_id": old.id,
            "new_appointment_id": new_appointment.id,
            "vet_id": old.vet_id,
        }),
    );

    Ok(new_appointment)
}

pub async fn confirm_appointment(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(id): PathParam<i64>,
) -> Result<StatusCode, AppError> {
    vet_transition(config, auth, id, AppointmentStatus::Confirmed).await
}

pub async fn no_show_appointment(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(id): PathParam<i64>,
) -> Result<StatusCode, AppError> {
    vet_transition(config, auth, id, AppointmentStatus::NoShow).await
}

/// `PUT /api/vet/appointments/{id}/confirm` and `.../no-show` — the target
/// contract declares both `200 OK` with no response body, unlike
/// `AppointmentResponse`-returning endpoints elsewhere in this slice.
async fn vet_transition(
    config: Config,
    auth: AuthUser,
    id: i64,
    next: AppointmentStatus,
) -> Result<StatusCode, AppError> {
    if auth.role != Role::Vet {
        return Err(AppError::Forbidden(
            "this endpoint requires the vet role".to_string(),
        ));
    }

    let data_dir = config.data_dir.clone();
    tokio::task::spawn_blocking(move || vet_transition_blocking(&data_dir, auth.id, id, next))
        .await
        .map_err(|_| AppError::Internal)??;

    Ok(StatusCode::OK)
}

fn vet_transition_blocking(
    data_dir: &Path,
    user_id: Uuid,
    wire_id: i64,
    next: AppointmentStatus,
) -> Result<(), AppError> {
    let vet_id = resolve_vet_id(data_dir, user_id)?;
    let located = model::find_by_wire_id(data_dir, wire_id)?.ok_or_else(appointment_not_found)?;
    if located.vet_id != vet_id {
        return Err(appointment_not_found());
    }

    let _lock = lock::acquire(data_dir, vet_id)?;

    let mut appointment =
        model::read_appointment(data_dir, vet_id, located.id)?.ok_or_else(appointment_not_found)?;

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
        AppointmentStatus::NoShow => "appointment_no_show",
        // `can_transition_to` above already rejects any other target this
        // function is called with.
        _ => "appointment_status_changed",
    };
    log_activity(
        data_dir,
        &resolve_username(data_dir, user_id),
        event_type,
        serde_json::json!({"appointment_id": appointment.id, "vet_id": vet_id}),
    );

    Ok(())
}

pub async fn list_owner_appointments(
    State(config): State<Config>,
    auth: AuthUser,
) -> Result<Json<Vec<AppointmentResponse>>, AppError> {
    if auth.role != Role::Owner {
        return Err(AppError::Forbidden(
            "this endpoint requires the owner role".to_string(),
        ));
    }

    let data_dir = config.data_dir.clone();
    let appointments =
        tokio::task::spawn_blocking(move || list_owner_appointments_blocking(&data_dir, auth.id))
            .await
            .map_err(|_| AppError::Internal)??;

    Ok(Json(
        appointments
            .into_iter()
            .map(AppointmentResponse::from)
            .collect(),
    ))
}

fn list_owner_appointments_blocking(
    data_dir: &Path,
    user_id: Uuid,
) -> Result<Vec<model::Appointment>, AppError> {
    let owner_id = resolve_owner_id(data_dir, user_id)?;
    let pet_ids: std::collections::HashSet<Uuid> = pets::list_pets_for_owner(data_dir, owner_id)?
        .into_iter()
        .map(|p| p.id)
        .collect();
    Ok(model::read_all(data_dir)?
        .into_iter()
        .filter(|a| pet_ids.contains(&a.pet_id))
        .collect())
}

pub async fn list_vet_appointments(
    State(config): State<Config>,
    auth: AuthUser,
) -> Result<Json<Vec<VetAppointmentResponse>>, AppError> {
    if auth.role != Role::Vet {
        return Err(AppError::Forbidden(
            "this endpoint requires the vet role".to_string(),
        ));
    }

    let data_dir = config.data_dir.clone();
    let responses =
        tokio::task::spawn_blocking(move || list_vet_appointments_blocking(&data_dir, auth.id))
            .await
            .map_err(|_| AppError::Internal)??;

    Ok(Json(responses))
}

fn list_vet_appointments_blocking(
    data_dir: &Path,
    user_id: Uuid,
) -> Result<Vec<VetAppointmentResponse>, AppError> {
    let vet_id = resolve_vet_id(data_dir, user_id)?;
    // Same read-path rationale as everywhere else a shared lock guards a
    // multi-file view (`docs/architecture-internals.md` §1).
    let _lock = crate::storage::FileLock::shared(&model::lock_path(data_dir, vet_id))?;

    model::read_all_for_vet(data_dir, vet_id)?
        .into_iter()
        .map(|a| to_vet_appointment_response(data_dir, a))
        .collect()
}

fn to_vet_appointment_response(
    data_dir: &Path,
    a: model::Appointment,
) -> Result<VetAppointmentResponse, AppError> {
    let pet = pets::read_pet(data_dir, a.pet_id)?.ok_or_else(appointment_not_found)?;
    let owner_username = resolve_owner_username(data_dir, pet.owner_id)?;

    Ok(VetAppointmentResponse {
        id: domain::wire_id(a.id),
        pet_id: domain::wire_id(pet.id),
        pet_name: pet.name,
        owner_username,
        starts_at: a.time_slot,
        status: a.status,
        appointment_type: a.appointment_type,
    })
}
