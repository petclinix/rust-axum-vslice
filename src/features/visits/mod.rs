//! A vet records/edits a visit summary on a confirmed appointment (which
//! completes it — see `handlers::put_vet_visit`); the owner reads visit
//! history for their pets.

mod handlers;
pub mod model;
#[cfg(test)]
mod tests;

use axum::Router;
use axum::routing::get;

use crate::config::Config;

pub fn router() -> Router<Config> {
    Router::new()
        .route(
            "/api/vet/visits/{id}",
            get(handlers::get_vet_visit).put(handlers::put_vet_visit),
        )
        .route(
            "/api/owner/pets/{id}/visits",
            get(handlers::list_owner_pet_visits),
        )
}
