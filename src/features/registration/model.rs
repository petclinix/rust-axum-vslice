//! User/Owner/Vet records and their file I/O (see `docs/architecture.md`'s
//! On-Disk Data Layout section). No cross-entity
//! abstraction — each function here talks to exactly one directory under
//! `data/`.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::Role;
use crate::storage;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub password_hash: String,
    pub role: Role,
    pub is_active: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_login: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Owner {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub phone: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Vet {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub specialty: String,
}

fn users_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("users")
}

fn user_path(data_dir: &Path, id: Uuid) -> PathBuf {
    users_dir(data_dir).join(format!("{id}.json"))
}

fn owners_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("owners")
}

fn owner_path(data_dir: &Path, id: Uuid) -> PathBuf {
    owners_dir(data_dir).join(format!("{id}.json"))
}

fn vets_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("vets")
}

fn vet_path(data_dir: &Path, id: Uuid) -> PathBuf {
    vets_dir(data_dir).join(format!("{id}.json"))
}

/// The registration slice's own lock: register-time username-uniqueness
/// check + write (see `docs/architecture.md`'s Design Constraints).
pub fn users_lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join("locks").join("users.lock")
}

pub fn write_user(data_dir: &Path, user: &User) -> io::Result<()> {
    storage::atomic_write(&user_path(data_dir, user.id), user)
}

pub fn read_user(data_dir: &Path, id: Uuid) -> io::Result<Option<User>> {
    storage::read_json(&user_path(data_dir, id))
}

/// Cross-slice: `admin::users` lists every account; `admin::stats` and the
/// admin-seeding startup check both need to know who already exists.
pub fn list_all_users(data_dir: &Path) -> io::Result<Vec<User>> {
    storage::list_dir_json(&users_dir(data_dir))
}

/// Case-insensitive — username uniqueness must not depend on how a client
/// capitalizes what it types in.
pub fn find_user_by_username(data_dir: &Path, username: &str) -> io::Result<Option<User>> {
    let users: Vec<User> = storage::list_dir_json(&users_dir(data_dir))?;
    Ok(users
        .into_iter()
        .find(|u| u.username.eq_ignore_ascii_case(username)))
}

pub fn write_owner(data_dir: &Path, owner: &Owner) -> io::Result<()> {
    storage::atomic_write(&owner_path(data_dir, owner.id), owner)
}

/// Cross-slice lookup: other slices (e.g. `pets`) key their records by
/// `owner_id`, not `user_id`, so this resolves "which owner is the
/// authenticated user" (`docs/architecture.md` Design Constraint 5 — a slice
/// calls another slice's public functions directly, not through a shared
/// repository).
pub fn find_owner_by_user_id(data_dir: &Path, user_id: Uuid) -> io::Result<Option<Owner>> {
    let owners: Vec<Owner> = storage::list_dir_json(&owners_dir(data_dir))?;
    Ok(owners.into_iter().find(|o| o.user_id == user_id))
}

pub fn write_vet(data_dir: &Path, vet: &Vet) -> io::Result<()> {
    storage::atomic_write(&vet_path(data_dir, vet.id), vet)
}

/// Cross-slice lookup: `availability` (and later `appointments`) key their
/// records by `vet_id`, not `user_id` — same rationale as
/// `find_owner_by_user_id`.
pub fn find_vet_by_user_id(data_dir: &Path, user_id: Uuid) -> io::Result<Option<Vet>> {
    let vets: Vec<Vet> = storage::list_dir_json(&vets_dir(data_dir))?;
    Ok(vets.into_iter().find(|v| v.user_id == user_id))
}

/// Cross-slice: `admin::stats` needs every vet (including ones with zero
/// appointments) to report a complete per-vet breakdown.
pub fn list_all_vets(data_dir: &Path) -> io::Result<Vec<Vet>> {
    storage::list_dir_json(&vets_dir(data_dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_user(username: &str) -> User {
        User {
            id: Uuid::new_v4(),
            username: username.to_string(),
            password_hash: "hash".to_string(),
            role: Role::Owner,
            is_active: true,
            created_at: OffsetDateTime::now_utc(),
            last_login: None,
        }
    }

    #[test]
    fn write_user_then_find_by_username_is_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let user = sample_user("Owner_Example");
        write_user(dir.path(), &user).unwrap();

        let found = find_user_by_username(dir.path(), "owner_example")
            .unwrap()
            .unwrap();

        assert_eq!(found, user);
    }

    #[test]
    fn find_user_by_username_no_match_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        write_user(dir.path(), &sample_user("a")).unwrap();

        assert_eq!(find_user_by_username(dir.path(), "nobody").unwrap(), None);
    }

    #[test]
    fn write_owner_and_vet_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let owner = Owner {
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            name: "Alice".to_string(),
            phone: "555-0100".to_string(),
        };
        let vet = Vet {
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            name: "Dr. Bob".to_string(),
            specialty: "Surgery".to_string(),
        };

        write_owner(dir.path(), &owner).unwrap();
        write_vet(dir.path(), &vet).unwrap();

        assert_eq!(
            storage::read_json::<Owner>(&owner_path(dir.path(), owner.id)).unwrap(),
            Some(owner)
        );
        assert_eq!(
            storage::read_json::<Vet>(&vet_path(dir.path(), vet.id)).unwrap(),
            Some(vet)
        );
    }

    #[test]
    fn find_owner_by_user_id_matches_and_no_match_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let user_id = Uuid::new_v4();
        let owner = Owner {
            id: Uuid::new_v4(),
            user_id,
            name: "Alice".to_string(),
            phone: "555-0100".to_string(),
        };
        write_owner(dir.path(), &owner).unwrap();

        assert_eq!(
            find_owner_by_user_id(dir.path(), user_id).unwrap(),
            Some(owner)
        );
        assert_eq!(
            find_owner_by_user_id(dir.path(), Uuid::new_v4()).unwrap(),
            None
        );
    }

    #[test]
    fn find_vet_by_user_id_matches_and_no_match_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let user_id = Uuid::new_v4();
        let vet = Vet {
            id: Uuid::new_v4(),
            user_id,
            name: "Dr. Bob".to_string(),
            specialty: "Surgery".to_string(),
        };
        write_vet(dir.path(), &vet).unwrap();

        assert_eq!(find_vet_by_user_id(dir.path(), user_id).unwrap(), Some(vet));
        assert_eq!(
            find_vet_by_user_id(dir.path(), Uuid::new_v4()).unwrap(),
            None
        );
    }

    #[test]
    fn read_user_by_id_and_list_all_users() {
        let dir = tempfile::tempdir().unwrap();
        let a = sample_user("a@example.com");
        let b = sample_user("b@example.com");
        write_user(dir.path(), &a).unwrap();
        write_user(dir.path(), &b).unwrap();

        assert_eq!(read_user(dir.path(), a.id).unwrap(), Some(a.clone()));
        assert_eq!(read_user(dir.path(), Uuid::new_v4()).unwrap(), None);

        let mut all = list_all_users(dir.path()).unwrap();
        all.sort_by_key(|u| u.id);
        let mut expected = vec![a, b];
        expected.sort_by_key(|u| u.id);
        assert_eq!(all, expected);
    }

    #[test]
    fn list_all_vets_returns_every_vet() {
        let dir = tempfile::tempdir().unwrap();
        let vet = Vet {
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            name: "Dr. Bob".to_string(),
            specialty: "Surgery".to_string(),
        };
        write_vet(dir.path(), &vet).unwrap();

        assert_eq!(list_all_vets(dir.path()).unwrap(), vec![vet]);
    }
}
