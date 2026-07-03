//! User configuration at `~/.config/ani-dl/config.toml`.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub download: DownloadConfig,
    pub api: ApiConfig,
    pub sync: SyncConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadConfig {
    pub directory: String,
    pub quality: String,
    pub concurrency: usize,
    pub retries: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiConfig {
    /// AES key hex, refreshed by the sync daemon when upstream rotates.
    pub allanime_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncConfig {
    pub interval_hours: u64,
    pub test_show_id: String,
    pub test_episode: String,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            // Defaults to the current directory (matches the CLI default).
            directory: ".".to_string(),
            quality: "best".to_string(),
            concurrency: 16,
            retries: 3,
        }
    }
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            allanime_key: crate::constants::allanime_key_hex(),
        }
    }
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            interval_hours: 24,
            // "One Piece" long-running show id used for provider health checks.
            test_show_id: "ReooPAxPMsHM4KPMY".to_string(),
            test_episode: "1".to_string(),
        }
    }
}

/// `~/.config/ani-dl` (created on demand).
pub fn config_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    Ok(home.join(".config").join("ani-dl"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

impl Config {
    /// Load config, creating a default file if none exists.
    pub fn load() -> Result<Config> {
        let path = config_path()?;
        if !path.exists() {
            let cfg = Config::default();
            cfg.save()?;
            return Ok(cfg);
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config = toml::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let dir = config_dir()?;
        fs::create_dir_all(&dir)?;
        let text = toml::to_string_pretty(self)?;
        fs::write(config_path()?, text)?;
        Ok(())
    }
}
