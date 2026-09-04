//! Stateless auth (see `docs/architecture.md`'s Auth Design section): argon2
//! password hashing + HS256 JWTs. No
//! sessions on disk, no refresh tokens.

pub mod extractor;
pub mod password;
pub mod token;

pub use extractor::AuthUser;
