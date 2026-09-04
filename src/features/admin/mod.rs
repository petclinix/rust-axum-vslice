//! List/deactivate any user, read the activity log, view stats — all
//! admin-only. `activity` is `pub` so other slices can call
//! `admin::activity::record(...)`; `users`/`stats` stay private, reached
//! only through this module's `router()`.

pub mod activity;
mod stats;
#[cfg(test)]
mod tests;
mod users;

use axum::Router;
use axum::routing::{get, post};

use crate::config::Config;

pub fn router() -> Router<Config> {
    Router::new()
        .route("/api/admin/users", get(users::list_users))
        .route(
            "/api/admin/users/{id}/deactivate",
            post(users::deactivate_user),
        )
        .route("/api/admin/activity", get(activity::list_activity))
        .route("/api/admin/stats", get(stats::get_stats))
}
