use rust_axum_vslice::auth::password;
use rust_axum_vslice::build_router;
use rust_axum_vslice::config::Config;
use rust_axum_vslice::domain::Role;
use rust_axum_vslice::features::registration::model as registration;
use std::net::SocketAddr;
use std::path::Path;
use time::OffsetDateTime;
use uuid::Uuid;

const SEEDED_ADMIN_EMAIL: &str = "admin@petclinix.local";
const SEEDED_ADMIN_PASSWORD: &str = "admin12345";

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let config = Config::from_env();
    std::fs::create_dir_all(&config.data_dir).expect("failed to create data dir");
    seed_admin_if_needed(&config.data_dir).expect("failed to seed admin account");

    let port = config.port;
    let app = build_router(config);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind listener");
    tracing::info!("listening on {addr}");

    axum::serve(listener, app).await.expect("server error");
}

/// Admin accounts are seeded, never self-registered (PLAN.md §7/§11) —
/// same fixed credentials `php-twig-mtier` seeds its admin with, for easy
/// side-by-side comparison across the PetcliniX implementations.
fn seed_admin_if_needed(data_dir: &Path) -> std::io::Result<()> {
    let already_seeded = registration::list_all_users(data_dir)?
        .iter()
        .any(|u| u.role == Role::Admin);
    if already_seeded {
        return Ok(());
    }

    let password_hash = password::hash(SEEDED_ADMIN_PASSWORD)
        .expect("hashing the seeded admin password should never fail");
    let admin = registration::User {
        id: Uuid::new_v4(),
        email: SEEDED_ADMIN_EMAIL.to_string(),
        password_hash,
        role: Role::Admin,
        is_active: true,
        created_at: OffsetDateTime::now_utc(),
        last_login: None,
    };
    registration::write_user(data_dir, &admin)?;
    tracing::info!(email = SEEDED_ADMIN_EMAIL, "seeded admin account");
    Ok(())
}
