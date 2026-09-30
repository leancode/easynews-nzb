use reqwest::Response;
use reqwest::header::{HeaderMap, HeaderValue, RANGE, USER_AGENT};

use crate::models::{SearchResponse, TicketFile};

const SEARCH_URL: &str = "https://members.easynews.com/2.0/search/solr-search/advanced";
/// Fixed per-file download base (farm `auto`, port `443`); stable independent of any search session.
const DOWNLOAD_BASE: &str = "https://members.easynews.com/dl/auto/443";
const USER_AGENT_VALUE: &str = concat!("easynews-nzb/", env!("CARGO_PKG_VERSION"));

pub struct EasynewsClient {
    http: reqwest::Client,
    username: String,
    password: String,
}

impl EasynewsClient {
    pub fn new(http: reqwest::Client, username: String, password: String) -> Self {
        EasynewsClient {
            http,
            username,
            password,
        }
    }

    pub async fn search(
        &self,
        query: &str,
        types: &[&str],
        page: u32,
        per_page: u32,
    ) -> anyhow::Result<SearchResponse> {
        let mut req = self
            .http
            .get(SEARCH_URL)
            .basic_auth(&self.username, Some(&self.password))
            .header(USER_AGENT, USER_AGENT_VALUE)
            .query(&[
                ("gps", query),
                ("sb", "1"),
                ("st", "adv"),
                ("sS", "0"),
                ("pby", &per_page.to_string()),
                ("pno", &page.to_string()),
            ]);
        for t in types {
            req = req.query(&[("fty[]", *t)]);
        }
        let resp = req.send().await?.error_for_status()?;
        let body: serde_json::Value = resp.json().await?;
        SearchResponse::from_json(&body)
    }

    fn auth_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
        headers
    }

    /// Stream a ticket file, resuming from `start_at` bytes if given. Uses the per-file `sig`
    /// form, which does not depend on any search session.
    pub async fn download(&self, file: &TicketFile, start_at: u64) -> reqwest::Result<Response> {
        let url = format!(
            "{DOWNLOAD_BASE}/{hash}{ext}/{name}{ext}",
            hash = file.hash,
            ext = file.extension,
            name = urlencoding::encode(&file.file_name),
        );
        let mut req = self
            .http
            .get(&url)
            .basic_auth(&self.username, Some(&self.password))
            .headers(self.auth_headers())
            .query(&[("sig", &file.sig)]);
        if start_at > 0 {
            req = req.header(RANGE, format!("bytes={start_at}-"));
        }
        req.send().await
    }
}
