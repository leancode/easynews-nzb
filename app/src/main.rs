mod api;
mod config;
mod easynews;
mod health;
mod jobs;
mod models;
mod newznab;
mod nzb;
mod sab;
mod state;
mod xml;

use std::sync::Arc;

use axum::Router;
use axum::routing::get;

use config::Config;
use state::{AppState, Store};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = Config::from_env()?;
    let port = config.port;

    // umask is process-wide; set it before anything (state file, job resumption, downloads)
    // creates a file, so every directory/file this process writes gets consistent permissions.
    unsafe {
        libc::umask(config.umask as libc::mode_t);
    }

    let mut store = Store::load(std::path::Path::new(&config.state_file))?;
    store.prune_expired_tickets(jobs::now());

    let http = reqwest::Client::builder()
        .build()
        .expect("failed to build the HTTP client");

    let state = Arc::new(AppState::new(config, http, store));

    jobs::resume_jobs_on_startup(state.clone()).await;

    let app = Router::new()
        .route("/api", get(api::search))
        .route("/api/nzb/{token}", get(api::nzb))
        .route("/sab/api", get(sab::handle).post(sab::handle))
        .route("/health", get(health::handle))
        .with_state(state);

    let addr = format!("0.0.0.0:{port}");
    tracing::info!(%addr, "easynews-nzb listening");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
