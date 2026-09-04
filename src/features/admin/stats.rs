//! # pets, appointments per vet — computed on demand by directory scan,
//! same trade-off as derived availability (see `docs/architecture.md`'s
//! Design Constraints): no `COUNT()` a database would give for free, but
//! also nothing to keep in sync.

use std::collections::HashMap;
use std::path::Path;

use axum::Json;
use axum::extract::State;
use serde::Serialize;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::config::Config;
use crate::domain::Role;
use crate::error::AppError;
use crate::features::appointments::model as appointments;
use crate::features::pets::model as pets;
use crate::features::registration::model as registration;

#[derive(Debug, Serialize)]
pub struct StatsResponse {
    pub total_pets: usize,
    /// Every vet appears here, including ones with zero appointments —
    /// derived from the vet list, not just whichever vet directories happen
    /// to exist under `appointments/`.
    pub appointments_per_vet: HashMap<Uuid, usize>,
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
    let total_pets = pets::list_all(data_dir)?.len();

    let mut appointments_per_vet = HashMap::new();
    for vet in registration::list_all_vets(data_dir)? {
        let count = appointments::read_all_for_vet(data_dir, vet.id)?.len();
        appointments_per_vet.insert(vet.id, count);
    }

    Ok(StatsResponse {
        total_pets,
        appointments_per_vet,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::AppointmentStatus;
    use time::macros::{date, datetime};

    #[test]
    fn stats_count_pets_and_break_down_appointments_by_vet() {
        let dir = tempfile::tempdir().unwrap();

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
        let vet_with_none = Uuid::new_v4();
        registration::write_vet(
            dir.path(),
            &registration::Vet {
                id: vet_with_appointments,
                user_id: Uuid::new_v4(),
                name: "Dr. Bob".to_string(),
                specialty: "Surgery".to_string(),
            },
        )
        .unwrap();
        registration::write_vet(
            dir.path(),
            &registration::Vet {
                id: vet_with_none,
                user_id: Uuid::new_v4(),
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
                time_slot: datetime!(2026-09-07 10:00),
                duration_minutes: 30,
                status: AppointmentStatus::Booked,
            },
        )
        .unwrap();

        let stats = get_stats_blocking(dir.path()).unwrap();

        assert_eq!(stats.total_pets, 1);
        assert_eq!(stats.appointments_per_vet[&vet_with_appointments], 1);
        assert_eq!(stats.appointments_per_vet[&vet_with_none], 0);
    }
}
