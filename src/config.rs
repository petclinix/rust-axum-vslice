use std::path::PathBuf;

/// Env-based app configuration. See PLAN.md §2/§6 — no config-management
/// framework, plain `std::env` reads at this scale.
pub struct Config {
    pub port: u16,
    pub data_dir: PathBuf,
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

        Self { port, data_dir }
    }
}
