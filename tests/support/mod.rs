//! Shared black-box test harness (PLAN.md §10): spawns a real instance of
//! the app — a real `TcpListener` on a random port, served by real
//! `axum::serve` — against a fresh temp data dir, and drives it purely
//! through HTTP via `reqwest`, the same way a real client would. This is
//! deliberately a different (stronger) guarantee than the in-process
//! `tower::ServiceExt::oneshot` tests colocated with each slice: it proves
//! the whole binary's wiring — `main`'s startup sequence, real TCP, real
//! header/body serialization — not just one `Router`'s behavior in memory.
//!
//! Each file directly under `tests/` compiles as its own independent test
//! binary, so this shared module gets compiled once per binary — and not
//! every binary calls every helper here (e.g. only `admin_flow.rs` calls
//! `admin_token`). `dead_code` would otherwise fire per-binary; that's
//! expected for a shared test-support module, not a real problem.
#![allow(dead_code)]

use rust_axum_vslice::config::Config;
use rust_axum_vslice::{build_router, seed_admin_if_needed};
use serde_json::{Value, json};

pub struct TestServer {
    base_url: String,
    client: reqwest::Client,
    // Held only for its `Drop` — keeps the temp dir alive for the test's
    // duration.
    _data_dir: tempfile::TempDir,
}

impl TestServer {
    /// Spawns a fresh instance with its own temp data dir and a JWT secret
    /// unique to this call, so tests never share state or forge each
    /// other's tokens even when run concurrently (the default for `cargo
    /// test`).
    pub async fn spawn() -> Self {
        let data_dir = tempfile::tempdir().expect("create temp data dir");
        seed_admin_if_needed(data_dir.path()).expect("seed admin account");

        let config = Config {
            port: 0,
            data_dir: data_dir.path().to_path_buf(),
            jwt_secret: uuid::Uuid::new_v4().to_string(),
            cancellation_cutoff_hours: 24,
            appointment_default_duration_min: 30,
        };

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a random port");
        let addr = listener.local_addr().expect("read bound address");
        let app = build_router(config);

        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("black-box test server crashed");
        });

        Self {
            base_url: format!("http://{addr}"),
            client: reqwest::Client::new(),
            _data_dir: data_dir,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Registers a user (merging `extra` — e.g. `{"role": "owner", "phone":
    /// "555-0100"}` — into the request body) then logs in, returning the
    /// bearer token. The password is fixed across every call; nothing here
    /// needs it to vary.
    pub async fn register_and_login(&self, email: &str, extra: Value) -> String {
        let mut body = json!({
            "email": email,
            "password": "correct horse",
            "name": "Test User",
        });
        merge(&mut body, extra);

        let register = self
            .client
            .post(self.url("/api/auth/register"))
            .json(&body)
            .send()
            .await
            .expect("register request failed");
        assert!(
            register.status().is_success(),
            "registration for {email} failed: {} {}",
            register.status(),
            register.text().await.unwrap_or_default()
        );

        let login: Value = self
            .client
            .post(self.url("/api/auth/login"))
            .json(&json!({"email": email, "password": "correct horse"}))
            .send()
            .await
            .expect("login request failed")
            .json()
            .await
            .expect("login response was not JSON");

        login["token"]
            .as_str()
            .expect("login response had no token")
            .to_string()
    }

    pub async fn admin_token(&self) -> String {
        let login: Value = self
            .client
            .post(self.url("/api/auth/login"))
            .json(&json!({"email": "admin@petclinix.local", "password": "admin12345"}))
            .send()
            .await
            .expect("admin login request failed")
            .json()
            .await
            .expect("admin login response was not JSON");

        login["token"]
            .as_str()
            .expect("admin login response had no token")
            .to_string()
    }
}

fn merge(base: &mut Value, extra: Value) {
    let (Some(base_obj), Some(extra_obj)) = (base.as_object_mut(), extra.as_object()) else {
        return;
    };
    for (key, value) in extra_obj {
        base_obj.insert(key.clone(), value.clone());
    }
}
