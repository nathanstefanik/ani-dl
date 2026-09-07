//! User configuration at `~/.config/ani-dl/config.toml`.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub download: DownloadConfig,
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
pub struct SyncConfig {
    pub interval_hours: u64,
    /// Search query used by `ani-dl sync` health check.
    pub test_query: String,
    pub test_episode: String,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            directory: ".".to_string(),
            quality: "best".to_string(),
            concurrency: 16,
            retries: 3,
        }
    }
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            interval_hours: 24,
            test_query: "cyberpunk edgerunners".to_string(),
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
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
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
