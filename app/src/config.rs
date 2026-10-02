use std::env;

use crate::newznab::Thresholds;

#[derive(Clone)]
pub struct Config {
    pub easynews_username: String,
    pub easynews_password: String,
    pub api_key: String,
    pub public_url: String,
    pub sab_url: Option<String>,
    pub sab_api_key: Option<String>,
    pub incomplete_dir: String,
    pub complete_dir: String,
    pub state_file: String,
    pub port: u16,
    pub umask: u32,
    pub thresholds: Thresholds,
}

fn env_var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.is_empty())
}

fn require(name: &str) -> anyhow::Result<String> {
    env_var(name).ok_or_else(|| anyhow::anyhow!("missing required environment variable {name}"))
}

/// Read an env var as whole kilobytes and convert to bytes; falls back to `default_bytes` if
/// unset or not a plain integer.
fn env_kb_or(name: &str, default_bytes: u64) -> u64 {
    env_var(name)
        .and_then(|v| v.parse::<u64>().ok())
        .map(|kb| kb * 1024)
        .unwrap_or(default_bytes)
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let sab_url = env_var("SAB_URL");
        let sab_api_key = env_var("SAB_API_KEY");
        if sab_url.is_some() != sab_api_key.is_some() {
            anyhow::bail!("SAB_URL and SAB_API_KEY must be set together");
        }
        Ok(Config {
            easynews_username: require("EASYNEWS_USERNAME")?,
            easynews_password: require("EASYNEWS_PASSWORD")?,
            api_key: require("API_KEY")?,
            public_url: require("PUBLIC_URL")?.trim_end_matches('/').to_string(),
            sab_url: sab_url.map(|s| s.trim_end_matches('/').to_string()),
            sab_api_key,
            incomplete_dir: env_var("INCOMPLETE_DIR")
                .unwrap_or_else(|| "/downloads/incomplete".into()),
            complete_dir: env_var("COMPLETE_DIR").unwrap_or_else(|| "/downloads/complete".into()),
            state_file: env_var("STATE_FILE").unwrap_or_else(|| "/config/state.json".into()),
            port: env_var("PORT").and_then(|v| v.parse().ok()).unwrap_or(8090),
            umask: env_var("UMASK")
                .and_then(|v| u32::from_str_radix(&v, 8).ok())
                .unwrap_or(0o002),
            thresholds: {
                let defaults = Thresholds::default();
                Thresholds {
                    min_movie_size: env_kb_or("MIN_MOVIE_SIZE_KB", defaults.min_movie_size),
                    min_tv_size: env_kb_or("MIN_TV_SIZE_KB", defaults.min_tv_size),
                    min_audio_size: env_kb_or("MIN_AUDIO_SIZE_KB", defaults.min_audio_size),
                    min_book_size: env_kb_or("MIN_BOOK_SIZE_KB", defaults.min_book_size),
                }
            },
        })
    }
}
