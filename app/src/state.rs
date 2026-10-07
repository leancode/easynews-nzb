use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, RwLock, Semaphore};

use crate::config::Config;
use crate::easynews::EasynewsClient;
use crate::models::{Job, SearchRecord, Ticket};

const TICKET_TTL_SECS: i64 = 48 * 3600;
const MAX_PARALLEL_DOWNLOADS: usize = 2;
/// Caps for the web UI's history views. Independent of `tickets`/`jobs`, which stay short-lived
/// for their own operational reasons (ticket TTL, SABnzbd clients deleting their own history).
const SEARCH_LOG_CAP: usize = 5000;
const DOWNLOAD_LOG_CAP: usize = 5000;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub tickets: HashMap<String, Ticket>,
    #[serde(default)]
    pub jobs: HashMap<String, Job>,
    #[serde(default)]
    pub cached_get_config: Option<Value>,
    #[serde(default)]
    pub cached_get_cats: Option<Value>,
    /// Durable history for the web UI. `download_log` mirrors every `Job` ever created, keyed
    /// by `nzo_id`, and survives `sab::handle_delete` removing the job from `jobs` once the
    /// SABnzbd client (Sonarr/Radarr/DroppedNeedle) has imported it and asks us to forget it.
    #[serde(default)]
    pub search_log: VecDeque<SearchRecord>,
    #[serde(default)]
    pub download_log: HashMap<String, Job>,
}

impl Store {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Store::default());
        }
        let data = std::fs::read(path)?;
        if data.is_empty() {
            return Ok(Store::default());
        }
        Ok(serde_json::from_slice(&data)?)
    }

    /// Atomic write: write to a sibling temp file, then rename over the target.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp_path = path.with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp_path)?;
            let data = serde_json::to_vec_pretty(self)?;
            f.write_all(&data)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    pub fn prune_expired_tickets(&mut self, now: i64) {
        self.tickets
            .retain(|_, t| now - t.created < TICKET_TTL_SECS);
    }

    pub fn push_search(&mut self, record: SearchRecord) {
        self.search_log.push_back(record);
        while self.search_log.len() > SEARCH_LOG_CAP {
            self.search_log.pop_front();
        }
    }

    /// Mirror a job's current state into the durable download log. Called at every status
    /// change so `download_log` stays current even though `jobs` can be cleared out from under
    /// it by `sab::handle_delete`.
    pub fn record_download(&mut self, job: &Job) {
        self.download_log.insert(job.nzo_id.clone(), job.clone());
        if self.download_log.len() > DOWNLOAD_LOG_CAP
            && let Some(oldest_id) = self
                .download_log
                .values()
                .min_by_key(|j| j.completed.unwrap_or(j.created))
                .map(|j| j.nzo_id.clone())
        {
            self.download_log.remove(&oldest_id);
        }
    }
}

/// Live, in-memory-only per-file byte counters for jobs that are currently downloading.
/// Not persisted: on restart, `queue` progress just resumes from the last persisted
/// `bytes_done` until fresh chunks arrive.
#[derive(Default)]
pub struct LiveProgress {
    inner: RwLock<HashMap<String, HashMap<String, u64>>>,
}

impl LiveProgress {
    pub async fn set(&self, nzo_id: &str, file_name: &str, bytes: u64) {
        let mut map = self.inner.write().await;
        map.entry(nzo_id.to_string())
            .or_default()
            .insert(file_name.to_string(), bytes);
    }

    pub async fn get(&self, nzo_id: &str, file_name: &str) -> Option<u64> {
        let map = self.inner.read().await;
        map.get(nzo_id).and_then(|m| m.get(file_name)).copied()
    }

    pub async fn clear_job(&self, nzo_id: &str) {
        self.inner.write().await.remove(nzo_id);
    }
}

pub struct AppState {
    pub config: Config,
    pub easynews: EasynewsClient,
    pub http: reqwest::Client,
    pub store: RwLock<Store>,
    pub progress: LiveProgress,
    download_semaphore: Semaphore,
    last_search: Mutex<Instant>,
}

impl AppState {
    pub fn new(config: Config, http: reqwest::Client, store: Store) -> Self {
        let easynews = EasynewsClient::new(
            http.clone(),
            config.easynews_username.clone(),
            config.easynews_password.clone(),
        );
        AppState {
            config,
            easynews,
            http,
            store: RwLock::new(store),
            progress: LiveProgress::default(),
            download_semaphore: Semaphore::new(MAX_PARALLEL_DOWNLOADS),
            last_search: Mutex::new(Instant::now() - Duration::from_secs(2)),
        }
    }

    pub async fn read_store<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Store) -> R,
    {
        let guard = self.store.read().await;
        f(&guard)
    }

    /// Mutate the in-memory store then persist the resulting snapshot to disk.
    pub async fn mutate_store<F, R>(&self, f: F) -> anyhow::Result<R>
    where
        F: FnOnce(&mut Store) -> R,
        R: Send + 'static,
    {
        let (result, snapshot) = {
            let mut guard = self.store.write().await;
            let result = f(&mut guard);
            (result, guard.clone())
        };
        let path = PathBuf::from(&self.config.state_file);
        tokio::task::spawn_blocking(move || snapshot.save(&path)).await??;
        Ok(result)
    }

    /// Easynews politeness: at most one search per second, process-wide.
    pub async fn throttle_search(&self) {
        let mut last = self.last_search.lock().await;
        let elapsed = last.elapsed();
        if elapsed < Duration::from_secs(1) {
            tokio::time::sleep(Duration::from_secs(1) - elapsed).await;
        }
        *last = Instant::now();
    }

    pub async fn acquire_download_permit(&self) -> tokio::sync::SemaphorePermit<'_> {
        self.download_semaphore
            .acquire()
            .await
            .expect("download semaphore never closed")
    }
}
