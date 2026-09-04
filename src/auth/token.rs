use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use crate::domain::Role;

const EXPIRY: Duration = Duration::hours(1);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Subject: the user id.
    pub sub: String,
    pub role: Role,
    /// Unix timestamp; `jsonwebtoken` checks this against "now" on decode.
    pub exp: i64,
}

/// Issues an HS256 JWT for `user_id`/`role`, 1h expiry — matches
/// the java implementation's expiry for side-by-side comparability.
pub fn issue(secret: &str, user_id: &str, role: Role) -> jsonwebtoken::errors::Result<String> {
    let claims = Claims {
        sub: user_id.to_string(),
        role,
        exp: (OffsetDateTime::now_utc() + EXPIRY).unix_timestamp(),
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
}

/// Verifies signature and expiry, returning the decoded claims.
pub fn verify(secret: &str, token: &str) -> jsonwebtoken::errors::Result<Claims> {
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )?;
    Ok(data.claims)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::errors::ErrorKind;

    #[test]
    fn issue_then_verify_round_trips_the_claims() {
        let token = issue("secret", "user-1", Role::Owner).unwrap();

        let claims = verify("secret", &token).unwrap();

        assert_eq!(claims.sub, "user-1");
        assert_eq!(claims.role, Role::Owner);
    }

    #[test]
    fn verify_with_the_wrong_secret_fails() {
        let token = issue("secret", "user-1", Role::Owner).unwrap();

        let err = verify("wrong-secret", &token).unwrap_err();

        assert_eq!(*err.kind(), ErrorKind::InvalidSignature);
    }

    #[test]
    fn verify_an_expired_token_fails() {
        let claims = Claims {
            sub: "user-1".to_string(),
            role: Role::Owner,
            exp: (OffsetDateTime::now_utc() - Duration::hours(1)).unix_timestamp(),
        };
        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(b"secret"),
        )
        .unwrap();

        let err = verify("secret", &token).unwrap_err();

        assert_eq!(*err.kind(), ErrorKind::ExpiredSignature);
    }
}
