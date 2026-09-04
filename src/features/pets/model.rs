//! Pet records and their file I/O. Picture bytes are
//! deliberately *not* part of this struct — see `uploads.rs`.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::Date;
use uuid::Uuid;

use crate::domain;
use crate::storage;

/// Closed enum on the wire (`docs/petclinix-openapi-snapshot.json`'s
/// `PetRequest`/`Pet`), replacing this repo's earlier free-text `type`
/// field — the target contract has no such field at all, `species` is the
/// whole of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Species {
    Dog,
    Cat,
    Bird,
    Rabbit,
    Reptile,
    #[default]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Gender {
    Male,
    Female,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pet {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub species: Species,
    pub breed: String,
    pub gender: Gender,
    pub birth_date: Date,
    pub picture_content_type: String,
    pub is_active: bool,
}

fn pets_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("pets")
}

fn pet_path(data_dir: &Path, id: Uuid) -> PathBuf {
    pets_dir(data_dir).join(format!("{id}.json"))
}

pub fn write_pet(data_dir: &Path, pet: &Pet) -> io::Result<()> {
    storage::atomic_write(&pet_path(data_dir, pet.id), pet)
}

pub fn read_pet(data_dir: &Path, id: Uuid) -> io::Result<Option<Pet>> {
    storage::read_json(&pet_path(data_dir, id))
}

pub fn list_pets_for_owner(data_dir: &Path, owner_id: Uuid) -> io::Result<Vec<Pet>> {
    let pets: Vec<Pet> = storage::list_dir_json(&pets_dir(data_dir))?;
    Ok(pets
        .into_iter()
        .filter(|p| p.owner_id == owner_id)
        .collect())
}

/// Resolves a wire id (`domain::wire_id`) back to the `Pet` it was derived
/// from, scoped to `owner_id` — the same directory scan `list_pets_for_owner`
/// already does, just with an extra filter, since nothing here keeps an
/// in-memory index between requests (`docs/architecture.md`'s Design
/// Constraints).
pub fn find_by_owner_and_wire_id(
    data_dir: &Path,
    owner_id: Uuid,
    wire_id: i64,
) -> io::Result<Option<Pet>> {
    let pets = list_pets_for_owner(data_dir, owner_id)?;
    Ok(pets.into_iter().find(|p| domain::wire_id(p.id) == wire_id))
}

/// Cross-slice: `admin::stats` needs the total pet count across every
/// owner.
pub fn list_all(data_dir: &Path) -> io::Result<Vec<Pet>> {
    storage::list_dir_json(&pets_dir(data_dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn sample_pet(owner_id: Uuid) -> Pet {
        Pet {
            id: Uuid::new_v4(),
            owner_id,
            name: "Rex".to_string(),
            species: Species::Dog,
            breed: "Labrador".to_string(),
            gender: Gender::Male,
            birth_date: date!(2020 - 01 - 15),
            picture_content_type: "image/jpeg".to_string(),
            is_active: true,
        }
    }

    #[test]
    fn write_pet_then_read_pet_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let pet = sample_pet(Uuid::new_v4());

        write_pet(dir.path(), &pet).unwrap();

        assert_eq!(read_pet(dir.path(), pet.id).unwrap(), Some(pet));
    }

    #[test]
    fn read_pet_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(read_pet(dir.path(), Uuid::new_v4()).unwrap(), None);
    }

    #[test]
    fn list_pets_for_owner_only_returns_that_owners_pets() {
        let dir = tempfile::tempdir().unwrap();
        let owner_a = Uuid::new_v4();
        let owner_b = Uuid::new_v4();
        let pet_a1 = sample_pet(owner_a);
        let pet_a2 = sample_pet(owner_a);
        let pet_b = sample_pet(owner_b);
        write_pet(dir.path(), &pet_a1).unwrap();
        write_pet(dir.path(), &pet_a2).unwrap();
        write_pet(dir.path(), &pet_b).unwrap();

        let mut found = list_pets_for_owner(dir.path(), owner_a).unwrap();
        found.sort_by_key(|p| p.id);
        let mut expected = vec![pet_a1, pet_a2];
        expected.sort_by_key(|p| p.id);

        assert_eq!(found, expected);
    }

    #[test]
    fn list_all_spans_every_owner() {
        let dir = tempfile::tempdir().unwrap();
        write_pet(dir.path(), &sample_pet(Uuid::new_v4())).unwrap();
        write_pet(dir.path(), &sample_pet(Uuid::new_v4())).unwrap();

        assert_eq!(list_all(dir.path()).unwrap().len(), 2);
    }

    #[test]
    fn species_and_gender_serialize_uppercase() {
        let pet = sample_pet(Uuid::new_v4());

        let json = serde_json::to_value(&pet).unwrap();

        assert_eq!(json["species"], "DOG");
        assert_eq!(json["gender"], "MALE");
    }

    #[test]
    fn find_by_owner_and_wire_id_matches_and_no_match_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let owner_id = Uuid::new_v4();
        let pet = sample_pet(owner_id);
        write_pet(dir.path(), &pet).unwrap();

        assert_eq!(
            find_by_owner_and_wire_id(dir.path(), owner_id, domain::wire_id(pet.id)).unwrap(),
            Some(pet)
        );
        assert_eq!(
            find_by_owner_and_wire_id(dir.path(), owner_id, 123456).unwrap(),
            None
        );
    }
}
