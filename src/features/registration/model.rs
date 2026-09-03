//! User/Owner/Vet records and their file I/O (PLAN.md §4). No cross-entity
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
    pub email: String,
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

fn owner_path(data_dir: &Path, id: Uuid) -> PathBuf {
    data_dir.join("owners").join(format!("{id}.json"))
}

fn vet_path(data_dir: &Path, id: Uuid) -> PathBuf {
    data_dir.join("vets").join(format!("{id}.json"))
}

/// The registration slice's own lock: register-time email-uniqueness check +
/// write (PLAN.md §3, "Non-critical writes").
pub fn users_lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join("locks").join("users.lock")
}

pub fn write_user(data_dir: &Path, user: &User) -> io::Result<()> {
    storage::atomic_write(&user_path(data_dir, user.id), user)
}

/// Case-insensitive — email uniqueness must not depend on how a client
/// capitalizes the address it types in.
pub fn find_user_by_email(data_dir: &Path, email: &str) -> io::Result<Option<User>> {
    let users: Vec<User> = storage::list_dir_json(&users_dir(data_dir))?;
    Ok(users
        .into_iter()
        .find(|u| u.email.eq_ignore_ascii_case(email)))
}

pub fn write_owner(data_dir: &Path, owner: &Owner) -> io::Result<()> {
    storage::atomic_write(&owner_path(data_dir, owner.id), owner)
}

pub fn write_vet(data_dir: &Path, vet: &Vet) -> io::Result<()> {
    storage::atomic_write(&vet_path(data_dir, vet.id), vet)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_user(email: &str) -> User {
        User {
            id: Uuid::new_v4(),
            email: email.to_string(),
            password_hash: "hash".to_string(),
            role: Role::Owner,
            is_active: true,
            created_at: OffsetDateTime::now_utc(),
            last_login: None,
        }
    }

    #[test]
    fn write_user_then_find_by_email_is_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let user = sample_user("Owner@Example.com");
        write_user(dir.path(), &user).unwrap();

        let found = find_user_by_email(dir.path(), "owner@example.com")
            .unwrap()
            .unwrap();

        assert_eq!(found, user);
    }

    #[test]
    fn find_user_by_email_no_match_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        write_user(dir.path(), &sample_user("a@example.com")).unwrap();

        assert_eq!(
            find_user_by_email(dir.path(), "nobody@example.com").unwrap(),
            None
        );
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
}
