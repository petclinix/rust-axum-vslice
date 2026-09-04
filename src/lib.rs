pub mod auth;
pub mod config;
pub mod domain;
pub mod error;
pub mod features;
pub mod storage;

use std::path::Path;

use axum::{Router, routing::get};
use time::OffsetDateTime;
use uuid::Uuid;

use auth::password;
use config::Config;
use domain::Role;
use features::registration::model as registration;

pub fn build_router(config: Config) -> Router {
    Router::new()
        .route("/health", get(health))
        .merge(features::registration::router())
        .merge(features::pets::router())
        .merge(features::availability::router())
        .merge(features::appointments::router())
        .merge(features::visits::router())
        .merge(features::admin::router())
        .merge(features::vets_directory::router())
        .with_state(config)
}

async fn health() -> &'static str {
    "ok"
}

const SEEDED_ADMIN_EMAIL: &str = "admin@petclinix.local";
const SEEDED_ADMIN_PASSWORD: &str = "admin12345";

/// Admin accounts are seeded, never self-registered (see `docs/architecture.md`'s
/// Auth Design section) — same
/// fixed credentials `php-twig-mtier` seeds its admin with, for easy
/// side-by-side comparison across the PetcliniX implementations. Exposed
/// from the library (not just called inline in `main`) so black-box tests
/// spawning the app via `build_router` go through the exact same startup
/// sequence the real binary does.
pub fn seed_admin_if_needed(data_dir: &Path) -> std::io::Result<()> {
    let already_seeded = registration::list_all_users(data_dir)?
        .iter()
        .any(|u| u.role == Role::Admin);
    if already_seeded {
        return Ok(());
    }

    let password_hash = password::hash(SEEDED_ADMIN_PASSWORD)
        .expect("hashing the seeded admin password should never fail");
    let admin = registration::User {
        id: Uuid::new_v4(),
        email: SEEDED_ADMIN_EMAIL.to_string(),
        password_hash,
        role: Role::Admin,
        is_active: true,
        created_at: OffsetDateTime::now_utc(),
        last_login: None,
    };
    registration::write_user(data_dir, &admin)?;
    tracing::info!(email = SEEDED_ADMIN_EMAIL, "seeded admin account");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_check_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let app = build_router(Config::for_test(dir.path().to_path_buf()));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn seed_admin_if_needed_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();

        seed_admin_if_needed(dir.path()).unwrap();
        seed_admin_if_needed(dir.path()).unwrap();

        let admins: Vec<_> = registration::list_all_users(dir.path())
            .unwrap()
            .into_iter()
            .filter(|u| u.role == Role::Admin)
            .collect();
        assert_eq!(admins.len(), 1);
        assert_eq!(admins[0].email, SEEDED_ADMIN_EMAIL);
    }
}
