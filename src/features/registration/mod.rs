//! Register + login, for both owner and vet roles — one slice, since it's
//! one end-user journey. Admin is seeded, never registered here.

mod handlers;
pub mod model;
#[cfg(test)]
mod tests;

use axum::Router;
use axum::routing::{get, post};

use crate::config::Config;

pub fn router() -> Router<Config> {
    Router::new()
        .route("/api/users/register", post(handlers::register))
        .route("/api/auth/login", post(handlers::login))
        .route("/api/users/aboutme", get(handlers::aboutme))
}
