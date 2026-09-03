//! The core slice (PLAN.md §5/§6): booking, the state machine, and the
//! `flock`-based concurrency this whole repo exists to demonstrate.

mod handlers;
mod lock;
pub mod model;
pub mod slots;
#[cfg(test)]
mod tests;

use axum::Router;
use axum::routing::{get, post};

use crate::config::Config;

pub fn router() -> Router<Config> {
    Router::new()
        .route("/api/vets/{id}/slots", get(handlers::get_slots))
        .route(
            "/api/appointments",
            post(handlers::book).get(handlers::list_mine),
        )
        .route("/api/appointments/{id}/cancel", post(handlers::cancel))
        .route(
            "/api/appointments/{id}/reschedule",
            post(handlers::reschedule),
        )
        .route("/api/appointments/{id}/confirm", post(handlers::confirm))
        .route("/api/appointments/{id}/complete", post(handlers::complete))
        .route("/api/appointments/{id}/no-show", post(handlers::no_show))
}
