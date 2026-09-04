use rust_axum_vslice::build_router;
use rust_axum_vslice::config::Config;
use rust_axum_vslice::seed_admin_if_needed;
use std::net::SocketAddr;

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
