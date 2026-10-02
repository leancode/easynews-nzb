use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, Method, StatusCode, header::CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::jobs;
use crate::models::JobStatus;
use crate::nzb;
use crate::state::AppState;

const SAB_VERSION_STANDALONE: &str = "5.1.3";

fn json_error(message: &str) -> Response {
    Json(json!({"status": false, "error": message})).into_response()
}

fn hhmmss(mut secs: u64) -> String {
    let h = secs / 3600;
    secs %= 3600;
    let m = secs / 60;
    let s = secs % 60;
    format!("{h}:{m:02}:{s:02}")
}

fn mb(bytes: u64) -> String {
    format!("{:.2}", bytes as f64 / 1024.0 / 1024.0)
}

pub async fn handle(
    State(state): State<Arc<AppState>>,
    method: Method,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let apikey = query.get("apikey").cloned().unwrap_or_default();
    if apikey != state.config.api_key {
        return (StatusCode::OK, json_error("API Key Incorrect")).into_response();
    }
    let mode = query.get("mode").cloned().unwrap_or_default();
    match mode.as_str() {
        "version" => handle_version(&state).await,
        "get_config" => handle_cacheable(&state, "get_config", "cached_get_config").await,
        "get_cats" => handle_cacheable(&state, "get_cats", "cached_get_cats").await,
        "addfile" => handle_addfile(&state, &query, &headers, body).await,
        "addurl" => forward(&state, method, query, headers, body).await,
        "queue" => handle_queue(&state, method, query, headers, body).await,
        "history" => handle_history(&state, method, query, headers, body).await,
        _ => forward(&state, method, query, headers, body).await,
    }
}

async fn handle_version(state: &Arc<AppState>) -> Response {
    if state.config.sab_url.is_some()
        && let Some(v) = forward_json(state, "version").await
    {
        return Json(v).into_response();
    }
    Json(json!({"version": SAB_VERSION_STANDALONE})).into_response()
}

async fn handle_cacheable(state: &Arc<AppState>, mode: &str, cache_field: &str) -> Response {
    if state.config.sab_url.is_some() {
        if let Some(v) = forward_json(state, mode).await {
            let v2 = v.clone();
            let _ = state
                .mutate_store(|s| {
                    if cache_field == "cached_get_config" {
                        s.cached_get_config = Some(v2.clone());
                    } else {
                        s.cached_get_cats = Some(v2.clone());
                    }
                })
                .await;
            return Json(v).into_response();
        }
        let cached = state
            .read_store(|s| {
                if cache_field == "cached_get_config" {
                    s.cached_get_config.clone()
                } else {
                    s.cached_get_cats.clone()
                }
            })
            .await;
        if let Some(v) = cached {
            return Json(v).into_response();
        }
    }
    if mode == "get_cats" {
        Json(json!({"categories": ["*", "movies", "tv", "music", "books"]})).into_response()
    } else {
        Json(standalone_config(state)).into_response()
    }
}

fn standalone_config(state: &Arc<AppState>) -> Value {
    json!({
        "config": {
            "misc": { "complete_dir": state.config.complete_dir },
            "categories": [
                {"name": "*", "priority": 0, "dir": ""},
                {"name": "movies", "priority": 0, "dir": "movies"},
                {"name": "tv", "priority": 0, "dir": "tv"},
                {"name": "music", "priority": 0, "dir": "music"},
                {"name": "books", "priority": 0, "dir": "books"},
            ],
        }
    })
}

/// GET `{mode}&output=json` from the real SABnzbd, authenticated with `SAB_API_KEY`.
async fn forward_json(state: &Arc<AppState>, mode: &str) -> Option<Value> {
    let sab_url = state.config.sab_url.as_ref()?;
    let key = state.config.sab_api_key.as_deref().unwrap_or_default();
    let url = format!("{sab_url}/api?mode={mode}&output=json&apikey={key}");
    let resp = state.http.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<Value>().await.ok()
}

async fn forward(
    state: &Arc<AppState>,
    method: Method,
    mut query: HashMap<String, String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(sab_url) = state.config.sab_url.clone() else {
        return json_error("no real SABnzbd is configured to forward to");
    };
    if let Some(key) = &state.config.sab_api_key {
        query.insert("apikey".to_string(), key.clone());
    }
    let qs = serde_urlencoded::to_string(&query).unwrap_or_default();
    let url = format!("{sab_url}/api?{qs}");
    let mut req = state.http.request(method, &url);
    if let Some(ct) = headers.get(CONTENT_TYPE) {
        req = req.header(CONTENT_TYPE, ct.clone());
    }
    if !body.is_empty() {
        req = req.body(body);
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let content_type = resp
                .headers()
                .get(CONTENT_TYPE)
                .cloned()
                .unwrap_or_else(|| axum::http::HeaderValue::from_static("application/json"));
            let bytes = resp.bytes().await.unwrap_or_default();
            let mut response = Response::new(axum::body::Body::from(bytes));
            *response.status_mut() =
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            response.headers_mut().insert(CONTENT_TYPE, content_type);
            response
        }
        Err(e) => json_error(&format!("forwarding to the real SABnzbd failed: {e}")),
    }
}

async fn handle_addfile(
    state: &Arc<AppState>,
    query: &HashMap<String, String>,
    headers: &HeaderMap,
    body: Bytes,
) -> Response {
    let content_type = match headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()) {
        Some(ct) => ct.to_string(),
        None => return json_error("addfile requires a multipart/form-data body"),
    };
    let boundary = match multer::parse_boundary(&content_type) {
        Ok(b) => b,
        Err(_) => return json_error("addfile requires a multipart/form-data body"),
    };

    let original_body = body.clone();
    let stream = futures_util::stream::once(async move { Ok::<_, std::io::Error>(body) });
    let mut multipart = multer::Multipart::new(stream, boundary);

    let mut cat = query.get("cat").cloned().unwrap_or_default();
    let mut priority = query.get("priority").cloned().unwrap_or_default();
    let mut nzbname = query.get("nzbname").cloned();
    let mut nzb_text: Option<String> = None;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(_) => return json_error("could not parse the multipart body"),
        };
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "name" | "nzbfile" => {
                nzb_text = field.text().await.ok();
            }
            "cat" => cat = field.text().await.unwrap_or_default(),
            "priority" => priority = field.text().await.unwrap_or_default(),
            "nzbname" => {
                let v = field.text().await.unwrap_or_default();
                if !v.is_empty() {
                    nzbname = Some(v);
                }
            }
            _ => {
                let _ = field.bytes().await;
            }
        }
    }

    let Some(nzb_text) = nzb_text else {
        return json_error("no NZB file field (name/nzbfile) in the upload");
    };

    match nzb::extract_ticket_token(&nzb_text) {
        Some(token) => {
            let ticket = state.read_store(|s| s.tickets.get(&token).cloned()).await;
            match ticket {
                Some(ticket) => {
                    let nzo_id = jobs::create_job(state, &ticket, cat, nzbname, priority).await;
                    Json(json!({"status": true, "nzo_ids": [nzo_id]})).into_response()
                }
                None => json_error("this ticket has expired; search again"),
            }
        }
        None => {
            // Not one of ours: forward the original multipart body unchanged.
            let mut fwd_query = HashMap::new();
            fwd_query.insert("mode".to_string(), "addfile".to_string());
            fwd_query.insert("output".to_string(), "json".to_string());
            forward(
                state,
                Method::POST,
                fwd_query,
                headers.clone(),
                original_body,
            )
            .await
        }
    }
}

async fn handle_queue(
    state: &Arc<AppState>,
    method: Method,
    query: HashMap<String, String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(name) = query.get("name") {
        if name == "delete" {
            return handle_delete(state, &query, true).await;
        }
        return forward(state, method, query, headers, body).await;
    }

    let mut base = if state.config.sab_url.is_some() {
        forward_json(state, "queue")
            .await
            .unwrap_or_else(default_queue_json)
    } else {
        default_queue_json()
    };

    let mut jobs: Vec<_> = state
        .read_store(|s| {
            s.jobs
                .values()
                .filter(|j| matches!(j.status, JobStatus::Queued | JobStatus::Downloading))
                .cloned()
                .collect::<Vec<_>>()
        })
        .await;
    jobs.sort_by_key(|j| j.created);

    let mut own_slots = Vec::new();
    for (index, job) in jobs.iter().enumerate() {
        let mut live_bytes = 0u64;
        for f in &job.files {
            let b = state
                .progress
                .get(&job.nzo_id, &f.file_name)
                .await
                .unwrap_or(f.bytes_done);
            live_bytes += b;
        }
        let mbleft = job.total_bytes.saturating_sub(live_bytes);
        own_slots.push(json!({
            "nzo_id": job.nzo_id,
            "filename": job.title,
            "cat": job.category,
            "status": if job.status == JobStatus::Downloading { "Downloading" } else { "Queued" },
            "percentage": jobs::Percentage::of(job, live_bytes).to_string(),
            "mb": mb(job.total_bytes),
            "mbleft": mb(mbleft),
            "sizeleft": mbleft.to_string(),
            "timeleft": hhmmss(0),
            "priority": job.priority,
            "index": index,
        }));
    }

    if let Some(nzo_ids) = query.get("nzo_ids") {
        let wanted: std::collections::HashSet<&str> = nzo_ids.split(',').collect();
        own_slots.retain(|s| wanted.contains(s["nzo_id"].as_str().unwrap_or("")));
    }

    merge_slots(&mut base, "queue", own_slots);
    apply_paging(&mut base, "queue", &query);
    Json(base).into_response()
}

async fn handle_history(
    state: &Arc<AppState>,
    method: Method,
    query: HashMap<String, String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(name) = query.get("name") {
        if name == "delete" {
            return handle_delete(state, &query, false).await;
        }
        return forward(state, method, query, headers, body).await;
    }

    let mut base = if state.config.sab_url.is_some() {
        forward_json(state, "history")
            .await
            .unwrap_or_else(default_history_json)
    } else {
        default_history_json()
    };

    let mut jobs: Vec<_> = state
        .read_store(|s| {
            s.jobs
                .values()
                .filter(|j| matches!(j.status, JobStatus::Completed | JobStatus::Failed))
                .cloned()
                .collect::<Vec<_>>()
        })
        .await;
    jobs.sort_by_key(|j| std::cmp::Reverse(j.completed.unwrap_or(j.created)));

    if let Some(cat) = query.get("category") {
        jobs.retain(|j| &j.category == cat);
    }

    let own_slots: Vec<Value> = jobs
        .iter()
        .map(|job| {
            json!({
                "nzo_id": job.nzo_id,
                "name": job.title,
                "nzb_name": format!("{}.nzb", job.title),
                "category": job.category,
                "status": if job.status == JobStatus::Completed { "Completed" } else { "Failed" },
                "storage": job.storage.clone().unwrap_or_default(),
                "bytes": job.total_bytes,
                "size": mb(job.total_bytes),
                "completed": job.completed.unwrap_or(job.created),
                "download_time": 0,
                "postproc_time": 0,
                "fail_message": job.fail_message.clone().unwrap_or_default(),
                "stage_log": [],
                "url": "",
            })
        })
        .collect();

    merge_slots(&mut base, "history", own_slots);
    apply_paging(&mut base, "history", &query);
    Json(base).into_response()
}

async fn handle_delete(
    state: &Arc<AppState>,
    query: &HashMap<String, String>,
    is_queue: bool,
) -> Response {
    let value = query.get("value").cloned().unwrap_or_default();
    let del_files = matches!(
        query.get("del_files").map(|s| s.as_str()),
        Some("1") | Some("true")
    );
    let mut ours = Vec::new();
    let mut theirs = Vec::new();
    for id in value.split(',').filter(|s| !s.is_empty()) {
        if id.starts_with("ez_") {
            ours.push(id.to_string());
        } else {
            theirs.push(id.to_string());
        }
    }

    for id in &ours {
        let job = state.read_store(|s| s.jobs.get(id).cloned()).await;
        if let Some(job) = job {
            if del_files {
                let dir = if job.status == JobStatus::Completed {
                    job.storage.clone()
                } else {
                    Some(
                        std::path::Path::new(&state.config.incomplete_dir)
                            .join(&job.folder)
                            .to_string_lossy()
                            .to_string(),
                    )
                };
                if let Some(dir) = dir {
                    let _ = tokio::fs::remove_dir_all(dir).await;
                }
            }
            let _ = state
                .mutate_store(|s| {
                    s.jobs.remove(id);
                })
                .await;
            state.progress.clear_job(id).await;
        }
    }

    if !theirs.is_empty() {
        let mut fwd_query = HashMap::new();
        fwd_query.insert(
            "mode".to_string(),
            if is_queue { "queue" } else { "history" }.to_string(),
        );
        fwd_query.insert("name".to_string(), "delete".to_string());
        fwd_query.insert("value".to_string(), theirs.join(","));
        if let Some(v) = query.get("del_files") {
            fwd_query.insert("del_files".to_string(), v.clone());
        }
        fwd_query.insert("output".to_string(), "json".to_string());
        return forward(
            state,
            Method::GET,
            fwd_query,
            HeaderMap::new(),
            Bytes::new(),
        )
        .await;
    }

    Json(json!({"status": true})).into_response()
}

fn default_queue_json() -> Value {
    json!({
        "queue": {
            "status": "Idle",
            "speed": "0 K/s",
            "paused": false,
            "noofslots": 0,
            "diskspace1": "0",
            "diskspace2": "0",
            "timeleft": "0:00:00",
            "slots": [],
        }
    })
}

fn default_history_json() -> Value {
    json!({
        "history": {
            "noofslots": 0,
            "slots": [],
        }
    })
}

fn merge_slots(base: &mut Value, section: &str, mut own_slots: Vec<Value>) {
    let pointer = format!("/{section}/slots");
    let existing = base
        .pointer(&pointer)
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    own_slots.extend(existing);
    let count = own_slots.len();
    if let Some(obj) = base.get_mut(section).and_then(|v| v.as_object_mut()) {
        obj.insert("slots".to_string(), Value::Array(own_slots));
        obj.insert("noofslots".to_string(), json!(count));
    } else {
        *base = json!({ section: { "slots": own_slots, "noofslots": count } });
    }
}

fn apply_paging(base: &mut Value, section: &str, query: &HashMap<String, String>) {
    let start: usize = query.get("start").and_then(|s| s.parse().ok()).unwrap_or(0);
    let limit: Option<usize> = query.get("limit").and_then(|s| s.parse().ok());
    if let Some(arr) = base
        .get_mut(section)
        .and_then(|v| v.as_object_mut())
        .and_then(|o| o.get_mut("slots"))
        .and_then(|v| v.as_array_mut())
    {
        let sliced: Vec<Value> = arr
            .iter()
            .skip(start)
            .take(limit.unwrap_or(usize::MAX))
            .cloned()
            .collect();
        *arr = sliced;
    }
}
