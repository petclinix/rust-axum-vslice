use std::path::Path;

use axum::Json;
use axum::extract::{Path as PathParam, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use time::Date;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::config::Config;
use crate::domain::{self, Role};
use crate::error::AppError;
use crate::features::registration::model as registration;

use super::model::{self, Gender, Species};
use super::uploads;

#[derive(Debug, Deserialize)]
pub struct PetRequest {
    pub name: String,
    #[serde(default)]
    pub species: Species,
    pub breed: String,
    #[serde(default)]
    pub gender: Gender,
    #[serde(rename = "birthDate")]
    pub birth_date: Date,
    /// Base64, no `data:` prefix — matches `java-springboot-react-mtier`'s
    /// wire contract (`docs/architecture-internals.md` §6).
    pub picture: String,
    #[serde(rename = "pictureContentType")]
    pub picture_content_type: String,
}

#[derive(Debug, Serialize)]
pub struct PetResponse {
    pub id: i64,
    pub name: String,
    pub species: Species,
    pub breed: String,
    pub gender: Gender,
    #[serde(rename = "birthDate")]
    pub birth_date: Date,
    pub picture: String,
    #[serde(rename = "pictureContentType")]
    pub picture_content_type: String,
    pub active: bool,
}

fn require_owner(auth: &AuthUser) -> Result<(), AppError> {
    if auth.role != Role::Owner {
        return Err(AppError::Forbidden(
            "this endpoint requires the owner role".to_string(),
        ));
    }
    Ok(())
}

fn validate_pet_request(req: &PetRequest) -> Result<(), AppError> {
    if req.name.trim().is_empty() {
        return Err(AppError::Validation("name is required".to_string()));
    }
    Ok(())
}

fn resolve_owner_id(data_dir: &Path, user_id: Uuid) -> Result<Uuid, AppError> {
    let owner = registration::find_owner_by_user_id(data_dir, user_id)?
        .ok_or_else(|| AppError::Forbidden("no owner profile for this account".to_string()))?;
    Ok(owner.id)
}

fn to_response(pet: model::Pet, picture: Vec<u8>) -> PetResponse {
    PetResponse {
        id: domain::wire_id(pet.id),
        name: pet.name,
        species: pet.species,
        breed: pet.breed,
        gender: pet.gender,
        birth_date: pet.birth_date,
        picture: uploads::encode_base64(&picture),
        picture_content_type: pet.picture_content_type,
        active: pet.is_active,
    }
}

pub async fn add_pet(
    State(config): State<Config>,
    auth: AuthUser,
    Json(req): Json<PetRequest>,
) -> Result<(StatusCode, Json<PetResponse>), AppError> {
    require_owner(&auth)?;
    validate_pet_request(&req)?;
    let picture_bytes = uploads::decode_and_validate(&req.picture, &req.picture_content_type)?;

    let data_dir = config.data_dir.clone();
    let response = tokio::task::spawn_blocking(move || {
        add_pet_blocking(&data_dir, auth.id, req, picture_bytes)
    })
    .await
    .map_err(|_| AppError::Internal)??;

    Ok((StatusCode::CREATED, Json(response)))
}

fn add_pet_blocking(
    data_dir: &Path,
    user_id: Uuid,
    req: PetRequest,
    picture_bytes: Vec<u8>,
) -> Result<PetResponse, AppError> {
    let owner_id = resolve_owner_id(data_dir, user_id)?;

    let pet = model::Pet {
        id: Uuid::new_v4(),
        owner_id,
        name: req.name,
        species: req.species,
        breed: req.breed,
        gender: req.gender,
        birth_date: req.birth_date,
        picture_content_type: req.picture_content_type.clone(),
        is_active: true,
    };
    model::write_pet(data_dir, &pet)?;
    uploads::write_picture(data_dir, pet.id, &req.picture_content_type, &picture_bytes)?;

    Ok(to_response(pet, picture_bytes))
}

pub async fn list_pets(
    State(config): State<Config>,
    auth: AuthUser,
) -> Result<Json<Vec<PetResponse>>, AppError> {
    require_owner(&auth)?;

    let data_dir = config.data_dir.clone();
    let pets = tokio::task::spawn_blocking(move || list_pets_blocking(&data_dir, auth.id))
        .await
        .map_err(|_| AppError::Internal)??;

    Ok(Json(pets))
}

fn list_pets_blocking(data_dir: &Path, user_id: Uuid) -> Result<Vec<PetResponse>, AppError> {
    let owner_id = resolve_owner_id(data_dir, user_id)?;
    let pets = model::list_pets_for_owner(data_dir, owner_id)?;

    pets.into_iter()
        .map(|pet| {
            let picture = uploads::read_picture(data_dir, pet.id, &pet.picture_content_type)?
                .unwrap_or_default();
            Ok(to_response(pet, picture))
        })
        .collect()
}

pub async fn get_pet(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(pet_id): PathParam<i64>,
) -> Result<Json<PetResponse>, AppError> {
    require_owner(&auth)?;

    let data_dir = config.data_dir.clone();
    let response =
        tokio::task::spawn_blocking(move || get_pet_blocking(&data_dir, auth.id, pet_id))
            .await
            .map_err(|_| AppError::Internal)??;

    Ok(Json(response))
}

fn get_pet_blocking(data_dir: &Path, user_id: Uuid, pet_id: i64) -> Result<PetResponse, AppError> {
    let owner_id = resolve_owner_id(data_dir, user_id)?;
    let pet = load_owned_pet(data_dir, owner_id, pet_id)?;
    let picture =
        uploads::read_picture(data_dir, pet.id, &pet.picture_content_type)?.unwrap_or_default();

    Ok(to_response(pet, picture))
}

pub async fn update_pet(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(pet_id): PathParam<i64>,
    Json(req): Json<PetRequest>,
) -> Result<Json<PetResponse>, AppError> {
    require_owner(&auth)?;
    validate_pet_request(&req)?;
    let picture_bytes = uploads::decode_and_validate(&req.picture, &req.picture_content_type)?;

    let data_dir = config.data_dir.clone();
    let response = tokio::task::spawn_blocking(move || {
        update_pet_blocking(&data_dir, auth.id, pet_id, req, picture_bytes)
    })
    .await
    .map_err(|_| AppError::Internal)??;

    Ok(Json(response))
}

fn update_pet_blocking(
    data_dir: &Path,
    user_id: Uuid,
    pet_id: i64,
    req: PetRequest,
    picture_bytes: Vec<u8>,
) -> Result<PetResponse, AppError> {
    let owner_id = resolve_owner_id(data_dir, user_id)?;
    let mut pet = load_owned_pet(data_dir, owner_id, pet_id)?;

    pet.name = req.name;
    pet.species = req.species;
    pet.breed = req.breed;
    pet.gender = req.gender;
    pet.birth_date = req.birth_date;
    pet.picture_content_type = req.picture_content_type.clone();

    model::write_pet(data_dir, &pet)?;
    uploads::write_picture(data_dir, pet.id, &req.picture_content_type, &picture_bytes)?;

    Ok(to_response(pet, picture_bytes))
}

pub async fn delete_pet(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(pet_id): PathParam<i64>,
) -> Result<StatusCode, AppError> {
    require_owner(&auth)?;

    let data_dir = config.data_dir.clone();
    tokio::task::spawn_blocking(move || delete_pet_blocking(&data_dir, auth.id, pet_id))
        .await
        .map_err(|_| AppError::Internal)??;

    Ok(StatusCode::OK)
}

/// Soft delete — flips `is_active` rather than removing the file, matching
/// the target contract's `Pet.active` flag and this repo's existing
/// soft-delete idiom for users (`admin::users::deactivate_user`).
/// Idempotent, same rationale as that handler.
fn delete_pet_blocking(data_dir: &Path, user_id: Uuid, pet_id: i64) -> Result<(), AppError> {
    let owner_id = resolve_owner_id(data_dir, user_id)?;
    let mut pet = load_owned_pet(data_dir, owner_id, pet_id)?;
    pet.is_active = false;
    model::write_pet(data_dir, &pet)?;
    Ok(())
}

/// Loads a pet by its wire id and checks it belongs to `owner_id` in one
/// place — a pet that exists but belongs to someone else must look
/// identical to a missing one, not leak its existence via a different
/// status code.
fn load_owned_pet(data_dir: &Path, owner_id: Uuid, pet_id: i64) -> Result<model::Pet, AppError> {
    model::find_by_owner_and_wire_id(data_dir, owner_id, pet_id)?.ok_or_else(pet_not_found)
}

fn pet_not_found() -> AppError {
    AppError::NotFound("pet not found".to_string())
}
