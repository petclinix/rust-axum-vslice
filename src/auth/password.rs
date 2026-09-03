use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use rand_core::OsRng;

/// Hashes `password` with a fresh random salt. The returned string is the
/// full PHC-format hash (algorithm + params + salt + hash) — store it as-is,
/// `verify` parses it back out.
pub fn hash(password: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default().hash_password(password.as_bytes(), &salt)?;
    Ok(hash.to_string())
}

/// Checks `password` against a hash previously produced by `hash`. Returns
/// `Ok(false)` for a simple mismatch; `Err` only for a malformed stored hash
/// (data corruption, not a wrong password), which callers should treat as an
/// internal error rather than "wrong password".
pub fn verify(password: &str, hash: &str) -> Result<bool, argon2::password_hash::Error> {
    let parsed = PasswordHash::new(hash)?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_then_verify_with_the_correct_password_succeeds() {
        let hashed = hash("correct horse battery staple").unwrap();

        assert!(verify("correct horse battery staple", &hashed).unwrap());
    }

    #[test]
    fn verify_with_the_wrong_password_fails_without_erroring() {
        let hashed = hash("correct horse battery staple").unwrap();

        assert!(!verify("wrong password", &hashed).unwrap());
    }

    #[test]
    fn two_hashes_of_the_same_password_differ() {
        // Proves each call salts independently rather than reusing one salt.
        let a = hash("correct horse battery staple").unwrap();
        let b = hash("correct horse battery staple").unwrap();

        assert_ne!(a, b);
    }

    #[test]
    fn verify_with_a_malformed_stored_hash_errors() {
        assert!(verify("anything", "not-a-valid-phc-hash").is_err());
    }
}
