use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use uuid::Uuid;

use crate::config::Config;
use crate::domain::Role;
use crate::error::AppError;

use super::token;

/// The authenticated caller, extracted from a verified bearer JWT (see
/// `docs/architecture.md`'s Auth Design section). Every protected handler
/// takes this as an argument; role checks are
/// plain `if auth.role != ...` per handler, not a declarative middleware
/// stack.
pub struct AuthUser {
    pub id: Uuid,
    pub role: Role,
}

impl FromRequestParts<Config> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Config,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .ok_or(AppError::Unauthenticated)?;

        let token = header
            .strip_prefix("Bearer ")
            .ok_or(AppError::Unauthenticated)?;

        let claims =
            token::verify(&state.jwt_secret, token).map_err(|_| AppError::Unauthenticated)?;

        let id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthenticated)?;

        Ok(AuthUser {
            id,
            role: claims.role,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt;

    fn app() -> Router {
        async fn whoami(auth: AuthUser) -> String {
            format!("{}:{:?}", auth.id, auth.role)
        }

        Router::new()
            .route("/whoami", get(whoami))
            .with_state(Config::for_test(std::env::temp_dir()))
    }

    fn request(auth_header: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().uri("/whoami");
        if let Some(value) = auth_header {
            builder = builder.header(AUTHORIZATION, value);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn missing_authorization_header_is_unauthenticated() {
        let response = app().oneshot(request(None)).await.unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn non_bearer_authorization_header_is_unauthenticated() {
        let response = app()
            .oneshot(request(Some("Basic dXNlcjpwYXNz")))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn malformed_token_is_unauthenticated() {
        let response = app()
            .oneshot(request(Some("Bearer not-a-jwt")))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn token_signed_with_a_different_secret_is_unauthenticated() {
        let user_id = Uuid::new_v4().to_string();
        let token = token::issue("a-different-secret", &user_id, "alice", Role::Owner).unwrap();

        let response = app()
            .oneshot(request(Some(&format!("Bearer {token}"))))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn valid_token_extracts_id_and_role() {
        let user_id = Uuid::new_v4();
        let token = token::issue("test-secret", &user_id.to_string(), "bob", Role::Vet).unwrap();

        let response = app()
            .oneshot(request(Some(&format!("Bearer {token}"))))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert_eq!(body, format!("{user_id}:Vet"));
    }
}
