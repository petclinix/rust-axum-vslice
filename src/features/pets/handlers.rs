use std::path::Path;

use axum::Json;
use axum::extract::{Path as PathParam, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use time::Date;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::config::Config;
use crate::domain::Role;
use crate::error::AppError;
use crate::features::registration::model as registration;

use super::{model, uploads};

#[derive(Debug, Deserialize)]
pub struct PetRequest {
    pub name: String,
    #[serde(rename = "type")]
    pub pet_type: String,
    pub breed: String,
    pub birth_date: Date,
    /// Base64, no `data:` prefix — matches `java-springboot-react-mtier`'s
    /// wire contract (PLAN.md §9).
    pub picture: String,
    #[serde(rename = "pictureContentType")]
    pub picture_content_type: String,
}

#[derive(Debug, Serialize)]
pub struct PetResponse {
    pub id: Uuid,
    pub name: String,
    #[serde(rename = "type")]
    pub pet_type: String,
    pub breed: String,
    pub birth_date: Date,
    pub picture: String,
    #[serde(rename = "pictureContentType")]
    pub picture_content_type: String,
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
    if req.pet_type.trim().is_empty() {
        return Err(AppError::Validation("type is required".to_string()));
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
        id: pet.id,
        name: pet.name,
        pet_type: pet.pet_type,
        breed: pet.breed,
        birth_date: pet.birth_date,
        picture: uploads::encode_base64(&picture),
        picture_content_type: pet.picture_content_type,
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
        pet_type: req.pet_type,
        breed: req.breed,
        birth_date: req.birth_date,
        picture_content_type: req.picture_content_type.clone(),
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
    PathParam(pet_id): PathParam<Uuid>,
) -> Result<Json<PetResponse>, AppError> {
    require_owner(&auth)?;

    let data_dir = config.data_dir.clone();
    // Visit history (PLAN.md §8's "includes visit history" note) arrives
    // once the `visits` slice exists (build order §13.7) — not yet.
    let response =
        tokio::task::spawn_blocking(move || get_pet_blocking(&data_dir, auth.id, pet_id))
            .await
            .map_err(|_| AppError::Internal)??;

    Ok(Json(response))
}

fn get_pet_blocking(data_dir: &Path, user_id: Uuid, pet_id: Uuid) -> Result<PetResponse, AppError> {
    let owner_id = resolve_owner_id(data_dir, user_id)?;
    let pet = load_owned_pet(data_dir, owner_id, pet_id)?;
    let picture =
        uploads::read_picture(data_dir, pet.id, &pet.picture_content_type)?.unwrap_or_default();

    Ok(to_response(pet, picture))
}

pub async fn update_pet(
    State(config): State<Config>,
    auth: AuthUser,
    PathParam(pet_id): PathParam<Uuid>,
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
    pet_id: Uuid,
    req: PetRequest,
    picture_bytes: Vec<u8>,
) -> Result<PetResponse, AppError> {
    let owner_id = resolve_owner_id(data_dir, user_id)?;
    let mut pet = load_owned_pet(data_dir, owner_id, pet_id)?;

    pet.name = req.name;
    pet.pet_type = req.pet_type;
    pet.breed = req.breed;
    pet.birth_date = req.birth_date;
    pet.picture_content_type = req.picture_content_type.clone();

    model::write_pet(data_dir, &pet)?;
    uploads::write_picture(data_dir, pet.id, &req.picture_content_type, &picture_bytes)?;

    Ok(to_response(pet, picture_bytes))
}

/// Loads a pet and checks it belongs to `owner_id` in one place — a pet that
/// exists but belongs to someone else must look identical to a missing one,
/// not leak its existence via a different status code.
fn load_owned_pet(data_dir: &Path, owner_id: Uuid, pet_id: Uuid) -> Result<model::Pet, AppError> {
    let pet = model::read_pet(data_dir, pet_id)?.ok_or_else(pet_not_found)?;
    if pet.owner_id != owner_id {
        return Err(pet_not_found());
    }
    Ok(pet)
}

fn pet_not_found() -> AppError {
    AppError::NotFound("pet not found".to_string())
}
