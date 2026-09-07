//! Provider health check (+ optional daily daemon).

use std::fs;
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::Serialize;

use crate::api::{AnidbClient, TranslationType};
use crate::config::{Config, config_dir};
use crate::providers::select_quality;
use crate::version;

#[derive(Debug, Serialize)]
pub struct HealthReport {
    /// Build that wrote this report — a stale file is otherwise hard to spot.
    pub ani_dl_version: String,
    pub ani_cli_parity: String,
    pub timestamp: String,
    pub query: String,
    pub show_id: String,
    pub show_name: String,
    pub episode: String,
    pub ok: bool,
    pub latency_ms: u128,
    pub stream_count: usize,
    pub sample_url: Option<String>,
    pub error: Option<String>,
}

fn log_line(msg: &str) -> Result<()> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir)?;
    let line = format!("[{}] {}\n", Utc::now().to_rfc3339(), msg);
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("sync.log"))?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

pub async fn run_once(cfg: &Config, verbose: bool) -> Result<()> {
    let report = health_check(cfg).await?;
    let dir = config_dir()?;
    fs::create_dir_all(&dir)?;
    let path = dir.join("provider_health.json");
    fs::write(&path, serde_json::to_string_pretty(&report)?)?;

    let summary = if report.ok {
        format!(
            "ok: {} ep {} — {} stream(s) in {}ms",
            report.show_name, report.episode, report.stream_count, report.latency_ms
        )
    } else {
        format!(
            "FAIL: {} — {}",
            report.show_name,
            report.error.as_deref().unwrap_or("unknown")
        )
    };
    log_line(&summary)?;
    if verbose {
        println!("{summary}");
        if let Some(url) = &report.sample_url {
            println!("  sample: {url}");
        }
        println!("wrote {}", path.display());
    }
    if !report.ok {
        anyhow::bail!("provider health check failed");
    }
    Ok(())
}

async fn health_check(cfg: &Config) -> Result<HealthReport> {
    let started = Instant::now();
    let api = AnidbClient::new()?;
    let query = &cfg.sync.test_query;
    let ep = &cfg.sync.test_episode;

    let result = async {
        let results = api.search(query, TranslationType::Sub).await?;
        let show = results
            .first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no search results for '{query}'"))?;
        let streams = api
            .episode_streams(&show.id, ep, TranslationType::Sub)
            .await?;
        let sample = select_quality(&streams, "best").map(|s| s.url.clone());
        Ok::<_, anyhow::Error>((show, streams.len(), sample))
    }
    .await;

    match result {
        Ok((show, stream_count, sample_url)) => Ok(HealthReport {
            ani_dl_version: version::short().to_string(),
            ani_cli_parity: version::ANI_CLI_PARITY.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            query: query.clone(),
            show_id: show.id,
            show_name: show.name,
            episode: ep.clone(),
            ok: stream_count > 0,
            latency_ms: started.elapsed().as_millis(),
            stream_count,
            sample_url,
            error: if stream_count == 0 {
                Some("no streams resolved".into())
            } else {
                None
            },
        }),
        Err(e) => Ok(HealthReport {
            ani_dl_version: version::short().to_string(),
            ani_cli_parity: version::ANI_CLI_PARITY.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            query: query.clone(),
            show_id: String::new(),
            show_name: String::new(),
            episode: ep.clone(),
            ok: false,
            latency_ms: started.elapsed().as_millis(),
            stream_count: 0,
            sample_url: None,
            error: Some(format!("{e:#}")),
        }),
    }
}

/// `ani-dl sync --daemon`: detach on first run, then loop every N hours.
pub async fn run_daemon(cfg: Config) -> Result<()> {
    if std::env::var("ANIDL_DAEMONIZED").is_err() {
        use std::process::{Command, Stdio};
        let exe = std::env::current_exe()?;
        let mut cmd = Command::new(exe);
        cmd.arg("sync")
            .arg("--daemon")
            .env("ANIDL_DAEMONIZED", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let child = cmd.spawn().context("spawning daemon child")?;
        let dir = config_dir()?;
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("sync.pid"), child.id().to_string())?;
        println!("ani-dl sync daemon started (pid {})", child.id());
        println!("Logs: {}", dir.join("sync.log").display());
        return Ok(());
    }

    log_line("daemon started")?;
    let interval_secs = cfg.sync.interval_hours.max(1) * 3600;
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Err(e) = run_once(&cfg, false).await {
                    log_line(&format!("health check error: {e:#}"))?;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                log_line("daemon stopped (SIGINT)")?;
                break;
            }
        }
    }
    Ok(())
}
