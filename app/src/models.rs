use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One file entry from an Easynews search result. Easynews returns both numeric column ids
/// and named duplicates for most fields; we read the named ones only (see docs/DESIGN.md section 1.1).
#[derive(Debug, Clone)]
pub struct EasynewsFile {
    pub hash: String,
    pub file_name: String,
    pub extension: String,
    pub size: u64,
    pub timestamp: i64,
    pub poster: String,
    pub groups: Vec<String>,
    pub file_type: String,
    pub fullres: Option<String>,
    pub setid: Option<String>,
    pub passwd: bool,
    pub virus: bool,
    pub sig: String,
}

fn as_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn as_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn as_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::String(s) => matches!(s.as_str(), "1" | "true" | "yes"),
        Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
        _ => false,
    }
}

fn field<'a>(obj: &'a serde_json::Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| obj.get(*k))
}

impl EasynewsFile {
    pub fn from_json(obj: &serde_json::Map<String, Value>) -> Option<Self> {
        let hash = field(obj, &["0", "hash"]).and_then(as_string)?;
        let file_name = field(obj, &["10", "fn"]).and_then(as_string)?;
        let extension = field(obj, &["11", "extension"]).and_then(as_string)?;
        let size = field(obj, &["rawSize", "size", "4"])
            .and_then(as_u64)
            .unwrap_or(0);
        let timestamp = field(obj, &["ts", "timestamp", "5"])
            .and_then(as_i64)
            .unwrap_or(0);
        let poster = field(obj, &["7", "poster"])
            .and_then(as_string)
            .unwrap_or_default();
        let groups = match field(obj, &["group_list", "9", "groups"]) {
            Some(Value::Array(items)) => items.iter().filter_map(as_string).collect(),
            Some(v) => as_string(v).into_iter().collect(),
            None => Vec::new(),
        };
        let file_type = field(obj, &["type"])
            .and_then(as_string)
            .unwrap_or_default();
        let fullres = field(obj, &["fullres", "3"]).and_then(as_string);
        let setid = field(obj, &["setid", "colid", "19"]).and_then(as_string);
        let passwd = field(obj, &["passwd", "password"])
            .map(as_bool)
            .unwrap_or(false);
        let virus = field(obj, &["virus"]).map(as_bool).unwrap_or(false);
        let sig = field(obj, &["sig"]).and_then(as_string).unwrap_or_default();
        Some(EasynewsFile {
            hash,
            file_name,
            extension,
            size,
            timestamp,
            poster,
            groups,
            file_type,
            fullres,
            setid,
            passwd,
            virus,
            sig,
        })
    }
}

#[derive(Debug, Clone)]
pub struct SearchResponse {
    pub data: Vec<EasynewsFile>,
}

impl SearchResponse {
    pub fn from_json(v: &Value) -> anyhow::Result<Self> {
        let obj = v
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("not an object"))?;
        let data = match obj.get("data") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|item| item.as_object())
                .filter_map(EasynewsFile::from_json)
                .collect(),
            _ => Vec::new(),
        };
        Ok(SearchResponse { data })
    }
}

/// A file kept inside a ticket, enough to redownload it and to rebuild the NZB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketFile {
    pub hash: String,
    pub extension: String,
    pub file_name: String,
    pub size: u64,
    pub sig: String,
}

/// A release Prowlarr was shown; the NZB it hands to the SABnzbd side just carries the token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub token: String,
    pub name: String,
    pub category: String,
    pub files: Vec<TicketFile>,
    pub created: i64,
    /// The Easynews query terms that found this release, so a job can re-search for a fresh `sig`.
    pub refresh_query: String,
    pub refresh_types: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobStatus {
    Queued,
    Downloading,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobFileProgress {
    pub file_name: String,
    pub extension: String,
    pub size: u64,
    pub bytes_done: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub nzo_id: String,
    pub ticket_token: String,
    pub title: String,
    pub category: String,
    pub folder: String,
    pub files: Vec<JobFileProgress>,
    pub total_bytes: u64,
    pub status: JobStatus,
    pub priority: String,
    pub created: i64,
    pub completed: Option<i64>,
    pub storage: Option<String>,
    pub fail_message: Option<String>,
}
