//! Totals + appointments-per-vet — computed on demand by directory scan,
//! same trade-off as derived availability was (see `docs/architecture.md`'s
//! Design Constraints): no `COUNT()` a database would give for free, but
//! also nothing to keep in sync.

use std::path::Path;

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::auth::AuthUser;
use crate::config::Config;
use crate::domain::Role;
use crate::error::AppError;
use crate::features::appointments::model as appointments;
use crate::features::pets::model as pets;
use crate::features::registration::model as registration;

#[derive(Debug, Serialize)]
pub struct VetAppointmentCount {
    #[serde(rename = "vetUsername")]
    pub vet_username: String,
    pub count: usize,
}

/// Matches the target contract's `StatsData` exactly
/// (`docs/petclinix-openapi-snapshot.json`).
#[derive(Debug, Serialize)]
pub struct StatsResponse {
    #[serde(rename = "totalOwners")]
    pub total_owners: usize,
    #[serde(rename = "totalVets")]
    pub total_vets: usize,
    #[serde(rename = "totalPets")]
    pub total_pets: usize,
    #[serde(rename = "totalAppointments")]
    pub total_appointments: usize,
    /// Every vet appears here, including ones with zero appointments —
    /// derived from the vet list, not just whichever vet directories happen
    /// to exist under `appointments/`.
    #[serde(rename = "appointmentsPerVet")]
    pub appointments_per_vet: Vec<VetAppointmentCount>,
}

pub async fn get_stats(
    State(config): State<Config>,
    auth: AuthUser,
) -> Result<Json<StatsResponse>, AppError> {
    if auth.role != Role::Admin {
        return Err(AppError::Forbidden(
            "this endpoint requires the admin role".to_string(),
        ));
    }

    let data_dir = config.data_dir.clone();
    let stats = tokio::task::spawn_blocking(move || get_stats_blocking(&data_dir))
        .await
        .map_err(|_| AppError::Internal)??;

    Ok(Json(stats))
}

fn get_stats_blocking(data_dir: &Path) -> Result<StatsResponse, AppError> {
    let total_owners = registration::list_all_owners(data_dir)?.len();
    let vets = registration::list_all_vets(data_dir)?;
    let total_vets = vets.len();
    let total_pets = pets::list_all(data_dir)?.len();
    let total_appointments = appointments::read_all(data_dir)?.len();

    let mut appointments_per_vet = Vec::with_capacity(vets.len());
    for vet in vets {
        let count = appointments::read_all_for_vet(data_dir, vet.id)?.len();
        let vet_username = registration::read_user(data_dir, vet.user_id)?
            .map(|u| u.username)
            .unwrap_or_default();
        appointments_per_vet.push(VetAppointmentCount {
            vet_username,
            count,
        });
    }

    Ok(StatsResponse {
        total_owners,
        total_vets,
        total_pets,
        total_appointments,
        appointments_per_vet,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AppointmentStatus, AppointmentType};
    use time::macros::{date, datetime};
    use uuid::Uuid;

    #[test]
    fn stats_count_totals_and_break_down_appointments_by_vet() {
        let dir = tempfile::tempdir().unwrap();

        let owner_user_id = Uuid::new_v4();
        registration::write_user(
            dir.path(),
            &registration::User {
                id: owner_user_id,
                username: "owner@example.com".to_string(),
                password_hash: "unused".to_string(),
                role: Role::Owner,
                is_active: true,
                created_at: time::OffsetDateTime::now_utc(),
                last_login: None,
            },
        )
        .unwrap();
        registration::write_owner(
            dir.path(),
            &registration::Owner {
                id: Uuid::new_v4(),
                user_id: owner_user_id,
                name: "Alice".to_string(),
                phone: "555-0100".to_string(),
            },
        )
        .unwrap();

        pets::write_pet(
            dir.path(),
            &pets::Pet {
                id: Uuid::new_v4(),
                owner_id: Uuid::new_v4(),
                name: "Rex".to_string(),
                species: pets::Species::Dog,
                breed: "Labrador".to_string(),
                gender: pets::Gender::Male,
                birth_date: date!(2020 - 01 - 01),
                picture_content_type: "image/jpeg".to_string(),
                is_active: true,
            },
        )
        .unwrap();

        let vet_with_appointments = Uuid::new_v4();
        let vet_with_appointments_user = Uuid::new_v4();
        let vet_with_none = Uuid::new_v4();
        let vet_with_none_user = Uuid::new_v4();
        registration::write_user(
            dir.path(),
            &registration::User {
                id: vet_with_appointments_user,
                username: "dr-bob".to_string(),
                password_hash: "unused".to_string(),
                role: Role::Vet,
                is_active: true,
                created_at: time::OffsetDateTime::now_utc(),
                last_login: None,
            },
        )
        .unwrap();
        registration::write_vet(
            dir.path(),
            &registration::Vet {
                id: vet_with_appointments,
                user_id: vet_with_appointments_user,
                name: "Dr. Bob".to_string(),
                specialty: "Surgery".to_string(),
            },
        )
        .unwrap();
        registration::write_user(
            dir.path(),
            &registration::User {
                id: vet_with_none_user,
                username: "dr-eve".to_string(),
                password_hash: "unused".to_string(),
                role: Role::Vet,
                is_active: true,
                created_at: time::OffsetDateTime::now_utc(),
                last_login: None,
            },
        )
        .unwrap();
        registration::write_vet(
            dir.path(),
            &registration::Vet {
                id: vet_with_none,
                user_id: vet_with_none_user,
                name: "Dr. Eve".to_string(),
                specialty: "Dentistry".to_string(),
            },
        )
        .unwrap();
        appointments::write_appointment(
            dir.path(),
            &appointments::Appointment {
                id: Uuid::new_v4(),
                pet_id: Uuid::new_v4(),
                vet_id: vet_with_appointments,
                location_id: Uuid::new_v4(),
                time_slot: datetime!(2026-09-07 10:00),
                duration_minutes: 30,
                status: AppointmentStatus::Booked,
                appointment_type: AppointmentType::Checkup,
            },
        )
        .unwrap();

        let stats = get_stats_blocking(dir.path()).unwrap();

        assert_eq!(stats.total_owners, 1);
        assert_eq!(stats.total_vets, 2);
        assert_eq!(stats.total_pets, 1);
        assert_eq!(stats.total_appointments, 1);
        let by_username: std::collections::HashMap<_, _> = stats
            .appointments_per_vet
            .iter()
            .map(|c| (c.vet_username.as_str(), c.count))
            .collect();
        assert_eq!(by_username["dr-bob"], 1);
        assert_eq!(by_username["dr-eve"], 0);
    }
}
