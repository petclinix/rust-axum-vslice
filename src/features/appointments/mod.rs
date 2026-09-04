//! The core slice: booking, the state machine, and the
//! `flock`-based concurrency this whole repo exists to demonstrate.

mod handlers;
mod lock;
pub mod model;
#[cfg(test)]
mod tests;

use axum::Router;
use axum::routing::{delete, get, post, put};

use crate::config::Config;

pub fn router() -> Router<Config> {
    Router::new()
        .route(
            "/api/owner/appointments",
            post(handlers::create_appointment).get(handlers::list_owner_appointments),
        )
        .route(
            "/api/owner/appointments/{id}",
            delete(handlers::cancel_owner_appointment),
        )
        .route(
            "/api/owner/appointments/{id}/reschedule",
            put(handlers::reschedule_appointment),
        )
        .route(
            "/api/vet/appointments",
            get(handlers::list_vet_appointments),
        )
        .route(
            "/api/vet/appointments/{id}",
            delete(handlers::cancel_vet_appointment),
        )
        .route(
            "/api/vet/appointments/{id}/confirm",
            put(handlers::confirm_appointment),
        )
        .route(
            "/api/vet/appointments/{id}/no-show",
            put(handlers::no_show_appointment),
        )
}
