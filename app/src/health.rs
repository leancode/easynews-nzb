use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use serde_json::json;

use crate::models::JobStatus;
use crate::state::AppState;

pub async fn handle(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let (tickets, queued, downloading, completed, failed) = state
        .read_store(|s| {
            let mut queued = 0;
            let mut downloading = 0;
            let mut completed = 0;
            let mut failed = 0;
            for j in s.jobs.values() {
                match j.status {
                    JobStatus::Queued => queued += 1,
                    JobStatus::Downloading => downloading += 1,
                    JobStatus::Completed => completed += 1,
                    JobStatus::Failed => failed += 1,
                }
            }
            (s.tickets.len(), queued, downloading, completed, failed)
        })
        .await;

    let sab_reachable = match &state.config.sab_url {
        None => None,
        Some(url) => {
            let ping = format!("{url}/api?mode=version&output=json");
            Some(
                state
                    .http
                    .get(&ping)
                    .send()
                    .await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false),
            )
        }
    };

    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "tickets": tickets,
        "jobs": {
            "queued": queued,
            "downloading": downloading,
            "completed": completed,
            "failed": failed,
        },
        "sab_reachable": sab_reachable,
    }))
}
