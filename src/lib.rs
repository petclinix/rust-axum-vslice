pub mod auth;
pub mod config;
pub mod domain;
pub mod error;
pub mod features;
pub mod storage;

use axum::{Router, routing::get};

use config::Config;

pub fn build_router(config: Config) -> Router {
    Router::new()
        .route("/health", get(health))
        .merge(features::registration::router())
        .merge(features::pets::router())
        .merge(features::availability::router())
        .with_state(config)
}

async fn health() -> &'static str {
    "ok"
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
}
