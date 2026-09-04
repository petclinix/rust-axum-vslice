//! Read-only: list vets + specialties, for owners picking who to book with.
//! No `model.rs` — this is a pure pass-through read of
//! `registration`'s `Vet` records, not a second source of truth for them.

mod handlers;
#[cfg(test)]
mod tests;

use axum::Router;
use axum::routing::get;

use crate::config::Config;

pub fn router() -> Router<Config> {
    Router::new().route("/api/vets", get(handlers::list_vets))
}
