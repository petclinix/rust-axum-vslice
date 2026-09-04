//! A vet's clinic locations: address + weekly opening periods + one-off
//! date overrides, full CRUD. Replaces the old per-vet `availability`
//! slice — see `model`'s doc comment for why. Two audiences: the owning
//! vet manages their own locations under `/api/locations`; owners
//! discover any vet's locations and free slots under `/api/owner/locations`.

mod handlers;
pub mod model;
pub mod slots;
#[cfg(test)]
mod tests;

use axum::Router;
use axum::routing::get;

use crate::config::Config;

pub fn router() -> Router<Config> {
    Router::new()
        .route(
            "/api/locations",
            get(handlers::list_locations).post(handlers::create_location),
        )
        .route(
            "/api/locations/{id}",
            get(handlers::get_location)
                .put(handlers::update_location)
                .delete(handlers::delete_location),
        )
        .route(
            "/api/owner/locations",
            get(handlers::list_bookable_locations),
        )
        .route(
            "/api/owner/locations/{id}/available-slots",
            get(handlers::available_slots),
        )
}
