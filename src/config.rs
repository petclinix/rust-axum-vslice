use std::path::PathBuf;

/// Env-based app configuration. See PLAN.md §2/§6 — no config-management
/// framework, plain `std::env` reads at this scale. Doubles as the axum
/// `State` shared with every handler that needs `data_dir` or `jwt_secret`.
#[derive(Clone)]
pub struct Config {
    pub port: u16,
    pub data_dir: PathBuf,
    pub jwt_secret: String,
}

impl Config {
    pub fn from_env() -> Self {
        let port = std::env::var("PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8080);

        let data_dir = std::env::var("DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./data"));

        // A fixed fallback keeps `cargo run`/local dev working with zero
        // setup, same as the sibling repos' compose files hardcoding a dev
        // secret — never rely on this default outside local dev.
        let jwt_secret = std::env::var("JWT_SECRET")
            .unwrap_or_else(|_| "insecure-dev-secret-change-me".to_string());

        Self {
            port,
            data_dir,
            jwt_secret,
        }
    }
}

#[cfg(test)]
impl Config {
    /// A `Config` pointed at `data_dir` (typically a fresh
    /// `tempfile::tempdir()`), for slice tests that need to build a router
    /// via `.with_state(...)` (PLAN.md §10).
    pub fn for_test(data_dir: PathBuf) -> Self {
        Self {
            port: 0,
            data_dir,
            jwt_secret: "test-secret".to_string(),
        }
    }
}
