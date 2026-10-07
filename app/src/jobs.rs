use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

use crate::models::{Job, JobFileProgress, JobStatus, Ticket};
use crate::state::AppState;

const FILE_MODE: u32 = 0o664;

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Strip characters that would escape the download folder or hide it.
pub fn sanitize_folder(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c == '/' || c == '\\' { '-' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    if cleaned.is_empty() {
        "release".to_string()
    } else {
        cleaned.to_string()
    }
}

/// Directory mode (2775 per docs/DESIGN.md) is deliberately left to the filesystem, not forced
/// here: INCOMPLETE_DIR/COMPLETE_DIR already carry `setgid` on this deployment, so new
/// directories inherit it and the right group automatically. Explicitly `chmod`-ing a directory
/// to add the setgid bit ourselves backfires when our process's gid doesn't match the inherited
/// group — Linux silently clears `setgid` on such a `chmod` (CAP_FSETID), which then breaks group
/// inheritance for every file written under it afterwards.
async fn ensure_dir(path: &Path) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(path).await?;
    Ok(())
}

/// Re-run the search that originally found this ticket to obtain a fresh `sig` for `hash`.
async fn refresh_sig(state: &AppState, ticket: &Ticket, hash: &str) -> anyhow::Result<String> {
    state.throttle_search().await;
    let types: Vec<&str> = ticket.refresh_types.iter().map(|s| s.as_str()).collect();
    let resp = state
        .easynews
        .search(&ticket.refresh_query, &types, 1, 100)
        .await?;
    resp.data
        .into_iter()
        .find(|f| f.hash == hash)
        .map(|f| f.sig)
        .ok_or_else(|| anyhow::anyhow!("file no longer found in a fresh search"))
}

async fn download_one_file(
    state: &AppState,
    nzo_id: &str,
    ticket: &mut Ticket,
    file_index: usize,
    dest_dir: &Path,
) -> anyhow::Result<()> {
    let dest_path = dest_dir.join(format!(
        "{}{}",
        ticket.files[file_index].file_name, ticket.files[file_index].extension
    ));

    let mut attempted_refresh = false;
    loop {
        let start_at = match tokio::fs::metadata(&dest_path).await {
            Ok(meta) => meta.len(),
            Err(_) => 0,
        };
        let file = ticket.files[file_index].clone();
        if start_at >= file.size && file.size > 0 {
            state.progress.set(nzo_id, &file.file_name, file.size).await;
            return Ok(());
        }

        let _permit = state.acquire_download_permit().await;
        let resp = state.easynews.download(&file, start_at).await?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            drop(_permit);
            if attempted_refresh {
                anyhow::bail!(
                    "Easynews refused the download twice (401/403) for {}",
                    file.file_name
                );
            }
            attempted_refresh = true;
            let fresh_sig = refresh_sig(state, ticket, &file.hash).await?;
            ticket.files[file_index].sig = fresh_sig;
            continue;
        }
        if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
            anyhow::bail!(
                "Easynews download failed with status {status} for {}",
                file.file_name
            );
        }

        let mut out = OpenOptions::new()
            .create(true)
            .write(true)
            .append(start_at > 0)
            .truncate(start_at == 0)
            .open(&dest_path)
            .await?;
        let mut written = start_at;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            out.write_all(&chunk).await?;
            written += chunk.len() as u64;
            state.progress.set(nzo_id, &file.file_name, written).await;
        }
        out.flush().await?;
        tokio::fs::set_permissions(&dest_path, std::fs::Permissions::from_mode(FILE_MODE)).await?;
        state.progress.set(nzo_id, &file.file_name, written).await;
        return Ok(());
    }
}

async fn move_dir(from: &Path, to: &Path) -> anyhow::Result<()> {
    if let Some(parent) = to.parent() {
        ensure_dir(parent).await?;
    }
    if tokio::fs::rename(from, to).await.is_ok() {
        return Ok(());
    }
    // Cross-device fallback: copy then remove.
    ensure_dir(to).await?;
    let mut entries = tokio::fs::read_dir(from).await?;
    while let Some(entry) = entries.next_entry().await? {
        let dest = to.join(entry.file_name());
        tokio::fs::copy(entry.path(), &dest).await?;
        tokio::fs::set_permissions(&dest, std::fs::Permissions::from_mode(FILE_MODE)).await?;
    }
    tokio::fs::remove_dir_all(from).await?;
    Ok(())
}

async fn run_job(state: Arc<AppState>, nzo_id: String) {
    let ticket = match state
        .read_store(|s| {
            s.jobs
                .get(&nzo_id)
                .and_then(|j| s.tickets.get(&j.ticket_token).cloned())
        })
        .await
    {
        Some(t) => t,
        None => {
            let _ = state
                .mutate_store(|s| {
                    if let Some(j) = s.jobs.get_mut(&nzo_id) {
                        j.status = JobStatus::Failed;
                        j.fail_message = Some("ticket expired before the download started".into());
                    }
                })
                .await;
            return;
        }
    };

    let folder = state
        .read_store(|s| s.jobs.get(&nzo_id).map(|j| j.folder.clone()))
        .await
        .unwrap_or_else(|| sanitize_folder(&ticket.name));
    let category = state
        .read_store(|s| s.jobs.get(&nzo_id).map(|j| j.category.clone()))
        .await
        .unwrap_or_default();

    let _ = state
        .mutate_store(|s| {
            if let Some(j) = s.jobs.get_mut(&nzo_id) {
                j.status = JobStatus::Downloading;
            }
            if let Some(job) = s.jobs.get(&nzo_id).cloned() {
                s.record_download(&job);
            }
        })
        .await;

    let incomplete_dir = PathBuf::from(&state.config.incomplete_dir).join(&folder);
    if let Err(e) = ensure_dir(&incomplete_dir).await {
        fail_job(
            &state,
            &nzo_id,
            format!("could not create incomplete folder: {e}"),
        )
        .await;
        return;
    }

    let mut ticket = ticket;
    for idx in 0..ticket.files.len() {
        if let Err(e) = download_one_file(&state, &nzo_id, &mut ticket, idx, &incomplete_dir).await
        {
            fail_job(&state, &nzo_id, e.to_string()).await;
            return;
        }
        let done_size = ticket.files[idx].size;
        let fname = ticket.files[idx].file_name.clone();
        let _ = state
            .mutate_store(|s| {
                if let Some(j) = s.jobs.get_mut(&nzo_id)
                    && let Some(fp) = j.files.iter_mut().find(|f| f.file_name == fname)
                {
                    fp.bytes_done = done_size;
                }
                s.tickets.insert(ticket.token.clone(), ticket.clone());
                if let Some(job) = s.jobs.get(&nzo_id).cloned() {
                    s.record_download(&job);
                }
            })
            .await;
    }

    let complete_dir = PathBuf::from(&state.config.complete_dir)
        .join(&category)
        .join(&folder);
    if let Err(e) = move_dir(&incomplete_dir, &complete_dir).await {
        fail_job(&state, &nzo_id, format!("could not move to complete: {e}")).await;
        return;
    }

    state.progress.clear_job(&nzo_id).await;
    let storage = complete_dir.to_string_lossy().to_string();
    let _ = state
        .mutate_store(|s| {
            if let Some(j) = s.jobs.get_mut(&nzo_id) {
                j.status = JobStatus::Completed;
                j.completed = Some(now());
                j.storage = Some(storage);
            }
            if let Some(job) = s.jobs.get(&nzo_id).cloned() {
                s.record_download(&job);
            }
        })
        .await;
}

async fn fail_job(state: &Arc<AppState>, nzo_id: &str, message: String) {
    tracing::warn!(nzo_id, %message, "download job failed");
    state.progress.clear_job(nzo_id).await;
    let _ = state
        .mutate_store(|s| {
            if let Some(j) = s.jobs.get_mut(nzo_id) {
                j.status = JobStatus::Failed;
                j.fail_message = Some(message);
            }
            if let Some(job) = s.jobs.get(nzo_id).cloned() {
                s.record_download(&job);
            }
        })
        .await;
}

pub fn spawn_job(state: Arc<AppState>, nzo_id: String) {
    tokio::spawn(run_job(state, nzo_id));
}

/// Build a new job for a ticket and queue it; returns the `nzo_id`.
pub async fn create_job(
    state: &Arc<AppState>,
    ticket: &Ticket,
    category: String,
    nzbname: Option<String>,
    priority: String,
    requested_by: String,
) -> String {
    let nzo_id = format!("ez_{}", ticket.token);
    let title = nzbname.unwrap_or_else(|| ticket.name.clone());
    let folder = sanitize_folder(&title);
    let files = ticket
        .files
        .iter()
        .map(|f| JobFileProgress {
            file_name: f.file_name.clone(),
            extension: f.extension.clone(),
            size: f.size,
            bytes_done: 0,
        })
        .collect();
    let total_bytes = ticket.files.iter().map(|f| f.size).sum();
    let job = Job {
        nzo_id: nzo_id.clone(),
        ticket_token: ticket.token.clone(),
        title,
        category,
        folder,
        files,
        total_bytes,
        status: JobStatus::Queued,
        priority,
        created: now(),
        completed: None,
        storage: None,
        fail_message: None,
        requested_by,
    };
    let _ = state
        .mutate_store(|s| {
            s.jobs.insert(nzo_id.clone(), job.clone());
            s.record_download(&job);
        })
        .await;
    spawn_job(state.clone(), nzo_id.clone());
    nzo_id
}

pub async fn resume_jobs_on_startup(state: Arc<AppState>) {
    let ids: Vec<String> = state
        .read_store(|s| {
            s.jobs
                .iter()
                .filter(|(_, j)| matches!(j.status, JobStatus::Downloading | JobStatus::Queued))
                .map(|(id, _)| id.clone())
                .collect()
        })
        .await;
    for id in ids {
        let _ = state
            .mutate_store(|s| {
                if let Some(j) = s.jobs.get_mut(&id) {
                    j.status = JobStatus::Queued;
                }
                if let Some(job) = s.jobs.get(&id).cloned() {
                    s.record_download(&job);
                }
            })
            .await;
        spawn_job(state.clone(), id);
    }
}

pub struct Percentage;

impl Percentage {
    pub fn of(job: &Job, live_bytes: u64) -> u32 {
        if job.total_bytes == 0 {
            return 100;
        }
        ((live_bytes as f64 / job.total_bytes as f64) * 100.0).min(100.0) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_folder_strips_slashes_and_leading_dots() {
        assert_eq!(sanitize_folder("Some/Release"), "Some-Release");
        assert_eq!(sanitize_folder("..hidden"), "hidden");
        assert_eq!(sanitize_folder("   "), "release");
        assert_eq!(
            sanitize_folder("Normal Title (2020)"),
            "Normal Title (2020)"
        );
    }
}
