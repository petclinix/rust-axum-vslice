//! A vet records diagnosis/vaccination/notes on a completed appointment;
//! the owner reads visit history for their pets (PLAN.md §6).

mod handlers;
pub mod model;
#[cfg(test)]
mod tests;

pub use handlers::VisitResponse;

use axum::Router;
use axum::routing::{get, post};

use crate::config::Config;

pub fn router() -> Router<Config> {
    Router::new()
        .route("/api/appointments/{id}/visit", post(handlers::record_visit))
        .route("/api/pets/{id}/visits", get(handlers::list_for_pet))
}
