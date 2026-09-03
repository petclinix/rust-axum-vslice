//! Pet records and their file I/O (PLAN.md §4). Picture bytes are
//! deliberately *not* part of this struct — see `uploads.rs`.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::Date;
use uuid::Uuid;

use crate::storage;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pet {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    #[serde(rename = "type")]
    pub pet_type: String,
    pub breed: String,
    pub birth_date: Date,
    pub picture_content_type: String,
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

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn sample_pet(owner_id: Uuid) -> Pet {
        Pet {
            id: Uuid::new_v4(),
            owner_id,
            name: "Rex".to_string(),
            pet_type: "dog".to_string(),
            breed: "Labrador".to_string(),
            birth_date: date!(2020 - 01 - 15),
            picture_content_type: "image/jpeg".to_string(),
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
    fn pet_type_serializes_as_bare_type_field() {
        let pet = sample_pet(Uuid::new_v4());

        let json = serde_json::to_value(&pet).unwrap();

        assert_eq!(json["type"], "dog");
        assert!(json.get("pet_type").is_none());
    }
}
