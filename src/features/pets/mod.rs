//! Add/list/get/update a pet, owner-scoped. Simplest CRUD
//! slice — proves the vertical-slice pattern end-to-end, including the
//! first protected route.

mod handlers;
pub mod model;
#[cfg(test)]
mod tests;
mod uploads;

use axum::Router;
use axum::routing::get;

use crate::config::Config;

pub fn router() -> Router<Config> {
    Router::new()
        .route(
            "/api/pets",
            get(handlers::list_pets).post(handlers::add_pet),
        )
        .route(
            "/api/pets/{id}",
            get(handlers::get_pet).put(handlers::update_pet),
        )
}
