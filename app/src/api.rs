use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::jobs;
use crate::models::Ticket;
use crate::newznab::{
    CAPS_XML, RssItem, SearchMode, build_releases, build_rss, easynews_query, ticket_token,
};
use crate::state::AppState;

fn xml_response(body: String) -> Response {
    let mut resp = Response::new(axum::body::Body::from(body));
    resp.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/xml; charset=utf-8"),
    );
    resp
}

fn error_xml(message: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><error code="100" description="{}"/>"#,
        crate::xml::escape(message)
    )
}

pub async fn search(
    State(state): State<Arc<AppState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let t = q.get("t").cloned().unwrap_or_default();
    if t == "caps" {
        return xml_response(CAPS_XML.to_string());
    }

    let apikey = q.get("apikey").cloned().unwrap_or_default();
    if apikey != state.config.api_key {
        return xml_response(error_xml("Incorrect API key"));
    }

    if t == "get" {
        let id = q.get("id").cloned().unwrap_or_default();
        return nzb(State(state), Path(id), Query(q)).await;
    }

    let mode = match t.as_str() {
        "movie" => SearchMode::Movie {
            year: q.get("year").cloned().filter(|s| !s.is_empty()),
        },
        "tvsearch" => SearchMode::TvSearch {
            season: q.get("season").and_then(|s| s.parse().ok()),
            ep: q.get("ep").and_then(|s| s.parse().ok()),
        },
        "music" => SearchMode::Music {
            artist: q.get("artist").cloned().filter(|s| !s.is_empty()),
            album: q.get("album").cloned().filter(|s| !s.is_empty()),
        },
        _ => SearchMode::Search,
    };

    let q_term = q.get("q").cloned();
    let (queries, types) = easynews_query(&mode, q_term.as_deref());
    let limit: usize = q
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50)
        .clamp(1, 100);
    let offset: usize = q.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);
    let page = (offset / 100) as u32 + 1;

    let mut all_files = Vec::new();
    for query_term in &queries {
        if query_term.trim().is_empty() {
            continue;
        }
        state.throttle_search().await;
        match state.easynews.search(query_term, &types, page, 100).await {
            Ok(resp) => {
                let found = !resp.data.is_empty();
                all_files.extend(resp.data);
                if found {
                    break;
                }
            }
            Err(e) => tracing::warn!(error = %e, query = %query_term, "easynews search failed"),
        }
    }

    let query_for_filter = queries.first().cloned().unwrap_or_default();
    let mut releases = build_releases(&mode, &query_for_filter, all_files);

    if let Some(cat_param) = q.get("cat").filter(|s| !s.is_empty()) {
        let wanted: std::collections::HashSet<u32> = cat_param
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        if !wanted.is_empty() {
            releases.retain(|r| wanted.contains(&r.category));
        }
    }

    let total = releases.len();
    let page_offset = offset % 100;
    let page_slice: Vec<_> = releases.into_iter().skip(page_offset).take(limit).collect();

    let refresh_query = queries.first().cloned().unwrap_or_default();
    let refresh_types: Vec<String> = types.iter().map(|s| s.to_string()).collect();

    let mut entries = Vec::new();
    for r in page_slice {
        let token = ticket_token(&r.files);
        let ticket = Ticket {
            token: token.clone(),
            name: r.title.clone(),
            category: r.category.to_string(),
            files: r.files.clone(),
            created: jobs::now(),
            refresh_query: refresh_query.clone(),
            refresh_types: refresh_types.clone(),
        };
        let _ = state
            .mutate_store(|s| {
                s.tickets.insert(token.clone(), ticket);
            })
            .await;
        let link = format!(
            "{}/api/nzb/{}?apikey={}",
            state.config.public_url,
            token,
            urlencoding::encode(&apikey)
        );
        entries.push((r, token, link));
    }

    let rss_items: Vec<RssItem> = entries
        .iter()
        .map(|(release, token, link)| RssItem {
            release,
            token,
            link: link.clone(),
        })
        .collect();

    xml_response(build_rss(&rss_items, total, offset))
}

pub async fn nzb(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let apikey = q.get("apikey").cloned().unwrap_or_default();
    if apikey != state.config.api_key {
        return xml_response(error_xml("Incorrect API key"));
    }
    match state.read_store(|s| s.tickets.get(&token).cloned()).await {
        Some(ticket) => {
            let body = crate::nzb::build_nzb(&ticket);
            let mut resp = Response::new(axum::body::Body::from(body));
            resp.headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static("application/x-nzb"));
            let filename = jobs::sanitize_folder(&ticket.name);
            if let Ok(value) =
                HeaderValue::from_str(&format!("attachment; filename=\"{filename}.nzb\""))
            {
                resp.headers_mut().insert(CONTENT_DISPOSITION, value);
            }
            resp
        }
        None => (StatusCode::NOT_FOUND, "ticket not found or expired").into_response(),
    }
}
