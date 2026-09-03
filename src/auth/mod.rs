//! Stateless auth (PLAN.md §7): argon2 password hashing + HS256 JWTs. No
//! sessions on disk, no refresh tokens.

pub mod password;
pub mod token;
