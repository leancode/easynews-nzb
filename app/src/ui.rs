use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;

use crate::logbuf;
use crate::state::AppState;

const PAGE_SIZE_DEFAULT: usize = 100;
const PAGE_SIZE_MAX: usize = 500;

static UI_HTML: &str = include_str!("../static/ui.html");

fn authorized(state: &AppState, query: &HashMap<String, String>) -> bool {
    query.get("apikey").map(String::as_str) == Some(state.config.api_key.as_str())
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "incorrect or missing apikey"})),
    )
        .into_response()
}

fn paging(query: &HashMap<String, String>) -> (usize, usize) {
    let page = query
        .get("page")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1);
    let page_size = query
        .get("page_size")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(PAGE_SIZE_DEFAULT)
        .clamp(1, PAGE_SIZE_MAX);
    (page, page_size)
}

pub async fn page() -> Response {
    ([(CONTENT_TYPE, "text/html; charset=utf-8")], UI_HTML).into_response()
}

pub async fn api_searches(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if !authorized(&state, &query) {
        return unauthorized();
    }
    let (page, page_size) = paging(&query);
    let mut items = state
        .read_store(|s| s.search_log.iter().cloned().collect::<Vec<_>>())
        .await;
    items.reverse(); // newest first
    let total = items.len();
    let start = (page - 1) * page_size;
    let page_items: Vec<_> = items.into_iter().skip(start).take(page_size).collect();
    Json(json!({"page": page, "page_size": page_size, "total": total, "items": page_items}))
        .into_response()
}

pub async fn api_downloads(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if !authorized(&state, &query) {
        return unauthorized();
    }
    let (page, page_size) = paging(&query);
    let mut items = state
        .read_store(|s| s.download_log.values().cloned().collect::<Vec<_>>())
        .await;
    items.sort_by_key(|j| std::cmp::Reverse(j.completed.unwrap_or(j.created)));
    let total = items.len();
    let start = (page - 1) * page_size;
    let page_items: Vec<_> = items.into_iter().skip(start).take(page_size).collect();
    Json(json!({"page": page, "page_size": page_size, "total": total, "items": page_items}))
        .into_response()
}

pub async fn api_logs(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if !authorized(&state, &query) {
        return unauthorized();
    }
    let lines = query
        .get("lines")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(2000)
        .clamp(1, 2000);
    Json(json!({"lines": logbuf::recent_lines(lines)})).into_response()
}
