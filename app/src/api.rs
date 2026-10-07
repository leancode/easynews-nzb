use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::jobs;
use crate::models::{SearchRecord, Ticket};
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

/// True when a `cat` query value names only TV categories (5xxx), so a plain search known to be
/// blank can be probed with a TV-shaped term instead of the generic fallback.
fn only_tv_categories(cat: Option<&String>) -> bool {
    let Some(cat) = cat else { return false };
    let ids: Vec<u32> = cat
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    !ids.is_empty() && ids.iter().all(|id| (5000..6000).contains(id))
}

/// True when a `cat` query value names only Movie categories (2xxx) — the Movie/TV counterpart
/// of `only_tv_categories`, used by `search_kind` to label plain searches more precisely.
fn only_movie_categories(cat: Option<&String>) -> bool {
    let Some(cat) = cat else { return false };
    let ids: Vec<u32> = cat
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    !ids.is_empty() && ids.iter().all(|id| (2000..3000).contains(id))
}

/// When a plain `t=search` is scoped (via `cat`) to exactly one of Movies/TV, Audio, or Books,
/// return the matching Easynews `fty[]` filter. Plain search normally asks Easynews for every
/// type so it can classify video as movie-vs-TV itself, but Easynews' own relevance ranking can
/// bury real matches behind non-video junk that happens to share the same text — found live
/// (2026-10-03): "Romeo Must Die 2000" with no type filter returned zero VIDEO files in the first
/// 100 raw results (88 were ARCHIVE, from old-style un-unpacked multi-part posts), even though 75
/// real VIDEO matches exist further down. Narrowing the Easynews-side query by `cat` avoids this.
fn cat_type_filter(cat: Option<&String>) -> Option<&'static str> {
    let cat = cat?;
    let category_type = |id: u32| match id {
        2000..=2999 | 5000..=5999 => Some("VIDEO"),
        3000..=3999 => Some("AUDIO"),
        7000..=7999 => Some("DOCUMENT"),
        _ => None,
    };
    let mut ids = cat.split(',').filter_map(|s| s.trim().parse().ok());
    let first = category_type(ids.next()?)?;
    ids.all(|id| category_type(id) == Some(first))
        .then_some(first)
}

/// What kind of content a search was for, for the web UI's history view — explicit search
/// modes (`t=movie`/`tvsearch`/`music`/`book`) say so directly; a plain `t=search` falls back to
/// `cat_type_filter`'s category narrowing, same logic used to scope the Easynews query itself.
fn search_kind(mode: &SearchMode, cat: Option<&String>) -> &'static str {
    match mode {
        SearchMode::Movie { .. } => "Movie",
        SearchMode::TvSearch { .. } => "TV",
        SearchMode::Music { .. } => "Music",
        SearchMode::Book { .. } => "Book",
        SearchMode::Search => match cat_type_filter(cat) {
            Some("VIDEO") if only_tv_categories(cat) => "TV",
            Some("VIDEO") if only_movie_categories(cat) => "Movie",
            Some("VIDEO") => "Movie/TV",
            Some("AUDIO") => "Music",
            Some("DOCUMENT") => "Book",
            _ => "General",
        },
    }
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
    headers: HeaderMap,
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
        "book" => SearchMode::Book {
            author: q.get("author").cloned().filter(|s| !s.is_empty()),
            title: q.get("title").cloned().filter(|s| !s.is_empty()),
        },
        _ => SearchMode::Search,
    };

    let q_term = q.get("q").cloned();
    // Prowlarr (and the apps it syncs indexers to) validate a plain `t=search` with no `q` but a
    // `cat` scoped to the app's own categories, and reject the indexer if that returns nothing in
    // those categories. Under `t=search`, movie/TV classification is a filename heuristic
    // (`SxxEyy`), so the generic blank-query fallback (the current year) rarely lands in the TV
    // categories by chance. When the request is blank and scoped to TV-only categories, probe
    // with an episode-shaped term instead so real results land where they're expected to.
    let effective_q = if matches!(mode, SearchMode::Search)
        && q_term.as_deref().map(str::trim).unwrap_or("").is_empty()
        && only_tv_categories(q.get("cat"))
    {
        Some("S01E01".to_string())
    } else {
        q_term.clone()
    };
    let (queries, mut types) = easynews_query(&mode, effective_q.as_deref());
    // Plain search has no type filter of its own; narrow it when `cat` unambiguously says what
    // kind of result is wanted (see `cat_type_filter`), so real matches aren't buried in the raw,
    // unrestricted search by non-video/audio/document junk sharing the same text.
    if matches!(mode, SearchMode::Search)
        && let Some(t) = cat_type_filter(q.get("cat"))
    {
        types = vec![t];
    }
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
    let mut releases = build_releases(
        &mode,
        &query_for_filter,
        all_files,
        &state.config.thresholds,
    );

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

    let client = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();
    let kind = search_kind(&mode, q.get("cat")).to_string();
    let _ = state
        .mutate_store(|s| {
            s.push_search(SearchRecord {
                ts: jobs::now(),
                client,
                kind,
                mode: t.clone(),
                query: q_term.clone(),
                cat: q.get("cat").cloned(),
                result_count: total,
            });
        })
        .await;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cat_type_filter_narrows_single_type_categories() {
        assert_eq!(
            cat_type_filter(Some(&"2030,2040,2045,2000".to_string())),
            Some("VIDEO")
        );
        assert_eq!(
            cat_type_filter(Some(&"5030,5040,5045,5000".to_string())),
            Some("VIDEO")
        );
        assert_eq!(
            cat_type_filter(Some(&"3010,3040".to_string())),
            Some("AUDIO")
        );
        assert_eq!(cat_type_filter(Some(&"7020".to_string())), Some("DOCUMENT"));
    }

    #[test]
    fn cat_type_filter_declines_mixed_or_absent_categories() {
        assert_eq!(cat_type_filter(None), None);
        assert_eq!(cat_type_filter(Some(&String::new())), None);
        // Movies + Audio together: genuinely ambiguous, must not guess.
        assert_eq!(cat_type_filter(Some(&"2000,3000".to_string())), None);
    }

    #[test]
    fn search_kind_prefers_the_explicit_mode_over_cat() {
        assert_eq!(
            search_kind(&SearchMode::Movie { year: None }, None),
            "Movie"
        );
        assert_eq!(
            search_kind(
                &SearchMode::TvSearch {
                    season: None,
                    ep: None
                },
                None
            ),
            "TV"
        );
        assert_eq!(
            search_kind(
                &SearchMode::Music {
                    artist: None,
                    album: None
                },
                None
            ),
            "Music"
        );
        assert_eq!(
            search_kind(
                &SearchMode::Book {
                    author: None,
                    title: None
                },
                None
            ),
            "Book"
        );
    }

    #[test]
    fn search_kind_falls_back_to_cat_for_plain_search() {
        let cat = |s: &str| Some(s.to_string());
        assert_eq!(
            search_kind(&SearchMode::Search, cat("5030,5040").as_ref()),
            "TV"
        );
        assert_eq!(
            search_kind(&SearchMode::Search, cat("2030,2040").as_ref()),
            "Movie"
        );
        assert_eq!(
            search_kind(&SearchMode::Search, cat("3010").as_ref()),
            "Music"
        );
        assert_eq!(
            search_kind(&SearchMode::Search, cat("7020").as_ref()),
            "Book"
        );
        assert_eq!(search_kind(&SearchMode::Search, None), "General");
    }
}
