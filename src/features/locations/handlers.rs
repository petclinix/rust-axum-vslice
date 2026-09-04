use std::path::Path;

use axum::Json;
use axum::extract::{Path as PathParam, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use time::{Date, Time};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::config::Config;
use crate::domain::{self, AppointmentType, Role, TimeRange};
use crate::error::AppError;
use crate::features::appointments::model as appointments;
use crate::features::registration::model as registration;
use crate::storage;

use super::model::{self, DayOfWeek};
use super::slots;

#[derive(Debug, Deserialize)]
pub struct OpeningPeriodInput {
    #[serde(rename = "dayOfWeek")]
    pub day_of_week: i32,
    #[serde(rename = "startTime")]
    pub start_time: Time,
    #[serde(rename = "endTime")]
    pub end_time: Time,
    #[serde(rename = "sortOrder", default)]
    pub sort_order: i32,
}

#[derive(Debug, Serialize)]
pub struct OpeningPeriodResponse {
    #[serde(rename = "dayOfWeek")]
    pub day_of_week: i32,
    #[serde(rename = "startTime")]
    pub start_time: Time,
    #[serde(rename = "endTime")]
    pub end_time: Time,
    #[serde(rename = "sortOrder")]
    pub sort_order: i32,
}

impl From<model::OpeningPeriod> for OpeningPeriodResponse {
    fn from(p: model::OpeningPeriod) -> Self {
        Self {
            day_of_week: p.day_of_week.iso_number(),
            start_time: p.start_time,
            end_time: p.end_time,
            sort_order: p.sort_order,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct OpeningOverrideInput {
    pub date: Date,
    #[serde(rename = "openTime", default)]
    pub open_time: Option<Time>,
    #[serde(rename = "closeTime", default)]
    pub close_time: Option<Time>,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct OpeningOverrideResponse {
    pub date: Date,
    #[serde(rename = "openTime")]
    pub open_time: Option<Time>,
    #[serde(rename = "closeTime")]
    pub close_time: Option<Time>,
    pub closed: bool,
    pub reason: String,
}

impl From<model::OpeningOverride> for OpeningOverrideResponse {
    fn from(o: model::OpeningOverride) -> Self {
        Self {
            date: o.date,
            open_time: o.open_time,
            close_time: o.close_time,
            closed: o.closed,
            reason: o.reason,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct LocationRequest {
    pub name: String,
    #[serde(rename = "zoneId", default)]
    pub zone_id: String,
    #[serde(default)]
    pub street: String,
    #[serde(rename = "postalCode", default)]
    pub postal_code: String,
    #[serde(default)]
    pub city: String,
    #[serde(default)]
    pub country: String,
    #[serde(rename = "weeklyPeriods", default)]
    pub weekly_periods: Vec<OpeningPeriodInput>,
    #[serde(default)]
    pub overrides: Vec<OpeningOverrideInput>,
}

#[derive(Debug, Serialize)]
pub struct LocationResponse {
    pub id: i64,
    pub name: String,
    #[serde(rename = "zoneId")]
    pub zone_id: String,
    pub street: String,
    #[serde(rename = "postalCode")]
    pub postal_code: String,
    pub city: String,
    pub country: String,
    #[serde(rename = "weeklyPeriods")]
    pub weekly_periods: Vec<OpeningPeriodResponse>,
    pub overrides: Vec<OpeningOverrideResponse>,
}

#[derive(Debug, Serialize)]
pub struct BookableLocationResponse {
    pub id: i64,
    pub name: String,
    #[serde(rename = "vetUsername")]
    pub vet_username: String,
    #[serde(rename = "zoneId")]
    pub zone_id: String,
    pub street: String,
    #[serde(rename = "postalCode")]
    pub postal_code: String,
    pub city: String,
    pub country: String,
}

#[derive(Debug, Serialize)]
pub struct AvailableSlotResponse {
    #[serde(rename = "startsAt")]
    pub starts_at: time::PrimitiveDateTime,
    #[serde(rename = "endsAt")]
    pub ends_at: time::PrimitiveDateTime,
}

#[derive(Debug, Deserialize)]
pub struct AvailableSlotsQuery {
    pub date: Date,
    /// Accepted and validated (an unknown value 422s, matching the target
    /// contract's closed enum) but not currently folded into slot
    /// duration — nothing in the domain model ties appointment type to a
    /// different length, and `appointments` doesn't carry this field yet
    /// (see `crate::domain::AppointmentType`'s doc comment).
    #[allow(dead_code)]
    #[serde(rename = "appointmentType")]
    pub appointment_type: AppointmentType,
}

fn require_vet(auth: &AuthUser) -> Result<(), AppError> {
    if auth.role != Role::Vet {
        return Err(AppError::Forbidden(
            "this endpoint requires the vet role".to_string(),
        ));
    }
    Ok(())
}

fn require_owner(auth: &AuthUser) -> Result<(), AppError> {
    if auth.role != Role::Owner {
        return Err(AppError::Forbidden(
            "this endpoint requires the owner role".to_string(),
        ));
    }
    Ok(())
}

fn resolve_vet_id(data_dir: &Path, user_id: Uuid) -> Result<Uuid, AppError> {
    let vet = registration::find_vet_by_user_id(data_dir, user_id)?
        .ok_or_else(|| AppError::Forbidden("no vet profile for this account".to_string()))?;
    Ok(vet.id)
}

/// Cross-slice: `Location.vet_id` is a `Vet` id, not a `User` id, so this
/// resolves it back to the username two lookups need to reach.
fn resolve_vet_username(data_dir: &Path, vet_id: Uuid) -> Result<String, AppError> {
    let vet = registration::list_all_vets(data_dir)?
        .into_iter()
        .find(|v| v.id == vet_id)
        .ok_or_else(|| AppError::NotFound("vet not found".to_string()))?;
    let user = registration::read_user(data_dir, vet.user_id)?
        .ok_or_else(|| AppError::NotFound("vet not found".to_string()))?;
    Ok(user.username)
}

fn location_not_found() -> AppError {
    AppError::NotFound("location not found".to_string())
}

fn validate_periods(
    periods: &[OpeningPeriodInput],
) -> Result<Vec<(DayOfWeek, &OpeningPeriodInput)>, AppError> {
    let mut resolved = Vec::with_capacity(periods.len());
    for period in periods {
        let day = DayOfWeek::from_iso_number(period.day_of_week).ok_or_else(|| {
            AppError::Validation("dayOfWeek must be between 1 (Monday) and 7 (Sunday)".to_string())
        })?;
        if period.start_time >= period.end_time {
            return Err(AppError::Validation(
                "startTime must be before endTime".to_string(),
            ));
        }
        resolved.push((day, period));
    }

    for i in 0..resolved.len() {
        for j in (i + 1)..resolved.len() {
            let (day_a, a) = resolved[i];
            let (day_b, b) = resolved[j];
            if day_a == day_b && a.start_time < b.end_time && b.start_time < a.end_time {
                return Err(AppError::Validation(format!(
                    "overlapping weekly periods on {day_a:?}"
                )));
            }
        }
    }

    Ok(resolved)
}

fn validate_overrides(overrides: &[OpeningOverrideInput]) -> Result<(), AppError> {
    for over in overrides {
        if over.closed {
            if over.open_time.is_some() || over.close_time.is_some() {
                return Err(AppError::Validation(
                    "openTime/closeTime must be omitted when closed is true".to_string(),
                ));
            }
        } else {
            match (over.open_time, over.close_time) {
                (Some(open), Some(close)) if open < close => {}
                (Some(_), Some(_)) => {
                    return Err(AppError::Validation(
                        "openTime must be before closeTime".to_string(),
                    ));
                }
                _ => {
                    return Err(AppError::Validation(
                        "openTime and closeTime are required when closed is false".to_string(),
                    ));
                }
            }
        }
    }

    for i in 0..overrides.len() {
        for j in (i + 1)..overrides.len() {
            if overrides[i].date == overrides[j].date {
                return Err(AppError::Validation(
                    "at most one override per date".to_string(),
                ));
            }
        }
    }

    Ok(())
}

fn to_response(
    location: model::Location,
    periods: Vec<model::OpeningPeriod>,
    overrides: Vec<model::OpeningOverride>,
) -> LocationResponse {
    LocationResponse {
        id: domain::wire_id(location.id),
        name: location.name,
        zone_id: location.zone_id,
        street: location.street,
        postal_code: location.postal_code,
        city: location.city,
        country: location.country,
        weekly_periods: periods
            .into_iter()
            .map(OpeningPeriodResponse::from)
            .collect(),
        overrides: overrides
            .into_iter()
            .map(OpeningOverrideResponse::from)
            .collect(),
    }
}

fn write_schedule(
    data_dir: &Path,
    location_id: Uuid,
    periods: &[OpeningPeriodInput],
    overrides: &[OpeningOverrideInput],
) -> Result<(), AppError> {
    model::delete_all_periods(data_dir, location_id)?;
    for input in periods {
        let day = DayOfWeek::from_iso_number(input.day_of_week)
            .expect("already validated by validate_periods");
        model::write_period(
            data_dir,
            &model::OpeningPeriod {
                id: Uuid::new_v4(),
                location_id,
                day_of_week: day,
                start_time: input.start_time,
                end_time: input.end_time,
                sort_order: input.sort_order,
            },
        )?;
    }

    model::delete_all_overrides(data_dir, location_id)?;
    for input in overrides {
        model::write_override(
            data_dir,
            &model::OpeningOverride {
                id: Uuid::new_v4(),
                location_id,
                date: input.date,
                open_time: input.open_time,
                close_time: input.close_time,
                closed: input.closed,
                reason: input.reason.clone(),
            },
        )?;
    }

    Ok(())
}

pub async fn create_location(
    State(config): State<Config>,
    auth: AuthUser,
    Json(req): Json<LocationRequest>,
) -> Result<(StatusCode, Json<LocationResponse>), AppError> {
    require_vet(&auth)?;
    if req.name.trim().is_empty() {
        return Err(AppError::Validation("name is required".to_string()));
    }
    validate_periods(&req.weekly_periods)?;
    validate_overrides(&req.overrides)?;

    let data_dir = config.data_dir.clone();
    let response =
        tokio::task::spawn_blocking(move || create_location_blocking(&data_dir, auth.id, req))
            .await
            .map_err(|_| AppError::Internal)??;

    Ok((StatusCode::CREATED, Json(response)))
}

fn create_location_blocking(
    data_dir: &Path,
    user_id: Uuid,
    req: LocationRequest,
) -> Result<LocationResponse, AppError> {
    let vet_id = resolve_vet_id(data_dir, user_id)?;
    let location_id = Uuid::new_v4();
    let _lock = storage::FileLock::exclusive(&model::lock_path(data_dir, location_id))?;

    let location = model::Location {
        id: location_id,
        vet_id,
        name: req.name.clone(),
        zone_id: req.zone_id.clone(),
        street: req.street.clone(),
        postal_code: req.postal_code.clone(),
        city: req.city.clone(),
        country: req.country.clone(),
    };
    model::write_location(data_dir, &location)?;
    write_schedule(data_dir, location_id, &req.weekly_periods, &req.overrides)?;

    let periods = model::read_periods(data_dir, location_id)?;
    let overrides = model::read_overrides(data_dir, location_id)?;
    Ok(to_response(location, periods, overrides))
}

pub async fn list_locations(
    State(config): State<Config>,
    auth: AuthUser,
) -> Result<Json<Vec<LocationResponse>>, AppError> {
    require_vet(&auth)?;

    let data_dir = config.data_dir.clone();
    let response = tokio::task::spawn_blocking(move || list_locations_blocking(&data_dir, auth.id))
        .await
        .map_err(|_| AppError::Internal)??;

    Ok(Json(response))
}

fn list_locations_blocking(
    data_dir: &Path,
    user_id: Uuid,
) -> Result<Vec<LocationResponse>, AppError> {
    let vet_id = resolve_vet_id(data_dir, user_id)?;
    let locations = model::list_locations_for_vet(data_dir, vet_id)?;

    locations
        .into_iter()
        .map(|location| {
            let periods = model::read_periods(data_dir, location.id)?;
            let overrides = model::read_overrides(data_dir, location.id)?;
            Ok(to_response(location, periods, overrides))
        })
        .collect()
}

pub async fn get_location(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(location_id): PathParam<i64>,
) -> Result<Json<LocationResponse>, AppError> {
    require_vet(&auth)?;

    let data_dir = config.data_dir.clone();
    let response =
        tokio::task::spawn_blocking(move || get_location_blocking(&data_dir, auth.id, location_id))
            .await
            .map_err(|_| AppError::Internal)??;

    Ok(Json(response))
}

fn get_location_blocking(
    data_dir: &Path,
    user_id: Uuid,
    location_id: i64,
) -> Result<LocationResponse, AppError> {
    let vet_id = resolve_vet_id(data_dir, user_id)?;
    let location = model::find_by_vet_and_wire_id(data_dir, vet_id, location_id)?
        .ok_or_else(location_not_found)?;
    let periods = model::read_periods(data_dir, location.id)?;
    let overrides = model::read_overrides(data_dir, location.id)?;
    Ok(to_response(location, periods, overrides))
}

pub async fn update_location(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(location_id): PathParam<i64>,
    Json(req): Json<LocationRequest>,
) -> Result<Json<LocationResponse>, AppError> {
    require_vet(&auth)?;
    if req.name.trim().is_empty() {
        return Err(AppError::Validation("name is required".to_string()));
    }
    validate_periods(&req.weekly_periods)?;
    validate_overrides(&req.overrides)?;

    let data_dir = config.data_dir.clone();
    let response = tokio::task::spawn_blocking(move || {
        update_location_blocking(&data_dir, auth.id, location_id, req)
    })
    .await
    .map_err(|_| AppError::Internal)??;

    Ok(Json(response))
}

fn update_location_blocking(
    data_dir: &Path,
    user_id: Uuid,
    location_id: i64,
    req: LocationRequest,
) -> Result<LocationResponse, AppError> {
    let vet_id = resolve_vet_id(data_dir, user_id)?;
    let mut location = model::find_by_vet_and_wire_id(data_dir, vet_id, location_id)?
        .ok_or_else(location_not_found)?;
    let _lock = storage::FileLock::exclusive(&model::lock_path(data_dir, location.id))?;

    location.name = req.name.clone();
    location.zone_id = req.zone_id.clone();
    location.street = req.street.clone();
    location.postal_code = req.postal_code.clone();
    location.city = req.city.clone();
    location.country = req.country.clone();
    model::write_location(data_dir, &location)?;
    write_schedule(data_dir, location.id, &req.weekly_periods, &req.overrides)?;

    let periods = model::read_periods(data_dir, location.id)?;
    let overrides = model::read_overrides(data_dir, location.id)?;
    Ok(to_response(location, periods, overrides))
}

pub async fn delete_location(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(location_id): PathParam<i64>,
) -> Result<StatusCode, AppError> {
    require_vet(&auth)?;

    let data_dir = config.data_dir.clone();
    tokio::task::spawn_blocking(move || delete_location_blocking(&data_dir, auth.id, location_id))
        .await
        .map_err(|_| AppError::Internal)??;

    Ok(StatusCode::OK)
}

fn delete_location_blocking(
    data_dir: &Path,
    user_id: Uuid,
    location_id: i64,
) -> Result<(), AppError> {
    let vet_id = resolve_vet_id(data_dir, user_id)?;
    let location = model::find_by_vet_and_wire_id(data_dir, vet_id, location_id)?
        .ok_or_else(location_not_found)?;
    let _lock = storage::FileLock::exclusive(&model::lock_path(data_dir, location.id))?;

    model::delete_location(data_dir, location.id)?;
    Ok(())
}

pub async fn list_bookable_locations(
    State(config): State<Config>,
    auth: AuthUser,
) -> Result<Json<Vec<BookableLocationResponse>>, AppError> {
    require_owner(&auth)?;

    let data_dir = config.data_dir.clone();
    let response = tokio::task::spawn_blocking(move || list_bookable_locations_blocking(&data_dir))
        .await
        .map_err(|_| AppError::Internal)??;

    Ok(Json(response))
}

fn list_bookable_locations_blocking(
    data_dir: &Path,
) -> Result<Vec<BookableLocationResponse>, AppError> {
    model::list_all_locations(data_dir)?
        .into_iter()
        .map(|location| {
            let vet_username = resolve_vet_username(data_dir, location.vet_id)?;
            Ok(BookableLocationResponse {
                id: domain::wire_id(location.id),
                name: location.name,
                vet_username,
                zone_id: location.zone_id,
                street: location.street,
                postal_code: location.postal_code,
                city: location.city,
                country: location.country,
            })
        })
        .collect()
}

pub async fn available_slots(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(location_id): PathParam<i64>,
    Query(query): Query<AvailableSlotsQuery>,
) -> Result<Json<Vec<AvailableSlotResponse>>, AppError> {
    require_owner(&auth)?;

    let data_dir = config.data_dir.clone();
    let response = tokio::task::spawn_blocking(move || {
        available_slots_blocking(&data_dir, location_id, query.date)
    })
    .await
    .map_err(|_| AppError::Internal)??;

    Ok(Json(response))
}

/// The location's busy ranges come from its vet's active appointments —
/// the closest approximation available before `appointments` itself
/// carries a `location_id` (a later phase): correct as long as each vet
/// has one location, over-blocks a vet running several. See
/// `crate::domain::AppointmentType`'s doc comment for the same caveat on
/// `appointmentType`.
fn available_slots_blocking(
    data_dir: &Path,
    location_id: i64,
    date: Date,
) -> Result<Vec<AvailableSlotResponse>, AppError> {
    let location = model::find_by_wire_id(data_dir, location_id)?.ok_or_else(location_not_found)?;

    let weekly = model::read_periods(data_dir, location.id)?;
    let over = model::find_override_by_date(data_dir, location.id, date)?;
    let busy: Vec<TimeRange> = appointments::read_active_for_vet(data_dir, location.vet_id)?
        .into_iter()
        .filter(|a| a.time_slot.date() == date)
        .map(|a| a.time_range())
        .collect();

    Ok(
        slots::derive_free_slots(date, &weekly, over.as_ref(), &busy)
            .into_iter()
            .map(|r| AvailableSlotResponse {
                starts_at: r.start,
                ends_at: r.end,
            })
            .collect(),
    )
}
