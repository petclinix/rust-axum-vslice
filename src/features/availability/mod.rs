//! A vet sets their recurring weekly schedule and one-off exceptions.
//! Both endpoints are vet-scoped writes on that vet's own
//! `availability-<vet_id>.lock` — no read endpoint here; the
//! `appointments` slice reads this data directly via `model::read_weekly`/
//! `read_exceptions` (`docs/architecture.md` Design Constraint 5) when it
//! derives free slots.

mod handlers;
pub mod model;
#[cfg(test)]
mod tests;

use axum::Router;
use axum::routing::post;

use crate::config::Config;

pub fn router() -> Router<Config> {
    Router::new()
        .route("/api/vets/availability", post(handlers::set_weekly))
        .route(
            "/api/vets/availability/exceptions",
            post(handlers::set_exception),
        )
}
