//! Daily sync: ani-cli source diff + provider health validation + daemon.

use std::fs;
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::Utc;
use regex::Regex;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::api::{AllAnimeClient, TranslationType};
use crate::config::{config_dir, Config};
use crate::constants::{allanime_key_hex, ANICLI_RAW_URL, KEY_SEED};
use crate::providers;

#[derive(Debug, Serialize)]
pub struct SyncReport {
    pub changed: bool,
    pub upstream_seed: Option<String>,
    pub key_rotated: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ProviderResult {
    pub source_name: String,
    pub ok: bool,
    pub latency_ms: u128,
    pub stream_count: usize,
    pub sample_url: Option<String>,
    pub format: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProviderHealthReport {
    pub timestamp: String,
    pub show_id: String,
    pub episode: String,
    pub providers: Vec<ProviderResult>,
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

/// Fetch upstream ani-cli, diff against the stored hash, and detect key/provider
/// changes.
pub async fn sync_anicli_source(cfg: &mut Config) -> Result<SyncReport> {
    let client = reqwest::Client::new();
    let source = client
        .get(ANICLI_RAW_URL)
        .send()
        .await
        .context("fetching ani-cli source")?
        .text()
        .await?;

    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    let hash = hex::encode(hasher.finalize());

    let dir = config_dir()?;
    fs::create_dir_all(&dir)?;
    let hash_path = dir.join("anicli_hash");
    let stored = fs::read_to_string(&hash_path).unwrap_or_default();

    let mut report = SyncReport {
        changed: stored.trim() != hash,
        upstream_seed: None,
        key_rotated: false,
        notes: Vec::new(),
    };

    // Extract the current key seed: allanime_key="$(printf '%s' 'SEED' | ...)"
    let seed_re = Regex::new(r#"allanime_key=.*printf '%s' '([^']+)'"#).unwrap();
    if let Some(cap) = seed_re.captures(&source) {
        let seed = cap[1].to_string();
        report.upstream_seed = Some(seed.clone());
        if seed != KEY_SEED {
            report.key_rotated = true;
            let new_key = {
                let mut h = Sha256::new();
                h.update(seed.as_bytes());
                hex::encode(h.finalize())
            };
            cfg.api.allanime_key = new_key.clone();
            cfg.save().ok();
            let note = format!(
                "KEY ROTATED: seed '{}' -> '{}' (new key {}). Update constants.rs KEY_SEED.",
                KEY_SEED, seed, &new_key[..12]
            );
            report.notes.push(note.clone());
            log_line(&note)?;
        }
    }

    // Diff a few provider hostnames of interest.
    for pat in ["allanime_base=", "youtu-chan.com", "fast4speed", "mp4upload"] {
        if !source.contains(pat) {
            let note = format!("PROVIDER PATTERN MISSING upstream: '{pat}'");
            report.notes.push(note.clone());
            log_line(&note)?;
        }
    }

    if report.changed {
        log_line(&format!("ani-cli source changed (hash {}...)", &hash[..12]))?;
        fs::write(&hash_path, &hash)?;
    } else {
        log_line("ani-cli source unchanged")?;
    }

    Ok(report)
}

/// Resolve every provider for a known test episode and record health.
pub async fn validate_providers(
    show_id: &str,
    ep: &str,
    mode: TranslationType,
) -> Result<ProviderHealthReport> {
    let api = AllAnimeClient::new()?;
    let sources = api.episode_sources(show_id, ep, mode).await?;

    let mut results = Vec::new();
    for src in &sources {
        let start = Instant::now();
        let resolved = providers::resolve(&api.client, src).await;
        let latency = start.elapsed().as_millis();
        match resolved {
            Ok(streams) => {
                let sample = streams.first().map(|s| s.url.clone());
                let format = sample.as_ref().map(|u| {
                    if u.contains(".m3u8") {
                        "m3u8".to_string()
                    } else if u.contains(".mp4") {
                        "mp4".to_string()
                    } else {
                        "other".to_string()
                    }
                });
                results.push(ProviderResult {
                    source_name: src.source_name.clone(),
                    ok: !streams.is_empty(),
                    latency_ms: latency,
                    stream_count: streams.len(),
                    sample_url: sample,
                    format,
                });
            }
            Err(_) => results.push(ProviderResult {
                source_name: src.source_name.clone(),
                ok: false,
                latency_ms: latency,
                stream_count: 0,
                sample_url: None,
                format: None,
            }),
        }
    }

    let report = ProviderHealthReport {
        timestamp: Utc::now().to_rfc3339(),
        show_id: show_id.to_string(),
        episode: ep.to_string(),
        providers: results,
    };

    let dir = config_dir()?;
    fs::create_dir_all(&dir)?;
    fs::write(
        dir.join("provider_health.json"),
        serde_json::to_string_pretty(&report)?,
    )?;
    Ok(report)
}

/// Run both checks once and print a human summary.
pub async fn run_once(cfg: &mut Config, print: bool) -> Result<()> {
    let sync = sync_anicli_source(cfg).await?;
    if print {
        println!("ani-cli source: {}", if sync.changed { "CHANGED" } else { "unchanged" });
        if let Some(seed) = &sync.upstream_seed {
            println!("  upstream key seed: {seed} (ours: {KEY_SEED})");
        }
        if sync.key_rotated {
            println!("  ⚠ KEY ROTATED — config updated; update constants.rs KEY_SEED");
        }
        for note in &sync.notes {
            println!("  note: {note}");
        }
    }

    let health =
        validate_providers(&cfg.sync.test_show_id, &cfg.sync.test_episode, TranslationType::Sub)
            .await?;
    if print {
        println!("\nProvider health (show {}, ep {}):", health.show_id, health.episode);
        for p in &health.providers {
            let status = if p.ok { "OK " } else { "FAIL" };
            println!(
                "  [{}] {:<12} {:>5}ms  {} streams  {}",
                status,
                p.source_name,
                p.latency_ms,
                p.stream_count,
                p.format.clone().unwrap_or_default(),
            );
        }
        let dir = config_dir()?;
        println!("\nWrote {}", dir.join("provider_health.json").display());
        println!("Wrote {}", dir.join("sync.log").display());
    }
    // Reference the hex helper so the current key is always logged.
    log_line(&format!("run_once complete (active key {}...)", &allanime_key_hex()[..12]))?;
    Ok(())
}

/// `ani-dl sync --daemon`: detach on first run, then loop every N hours.
pub async fn run_daemon(mut cfg: Config) -> Result<()> {
    if std::env::var("ANIDL_DAEMONIZED").is_err() {
        // Parent process: spawn a detached child and return immediately.
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
            cmd.process_group(0); // new process group: survives terminal close
        }
        let child = cmd.spawn().context("spawning daemon child")?;
        let dir = config_dir()?;
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("sync.pid"), child.id().to_string())?;
        println!("ani-dl sync daemon started (pid {})", child.id());
        println!("Logs: {}", dir.join("sync.log").display());
        return Ok(());
    }

    // Child process: run the loop until a termination signal arrives.
    log_line("daemon started")?;
    let interval_secs = cfg.sync.interval_hours.max(1) * 3600;
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Err(e) = run_once(&mut cfg, false).await {
                    log_line(&format!("sync error: {e}"))?;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                log_line("daemon received SIGINT, shutting down")?;
                break;
            }
            _ = terminate_signal() => {
                log_line("daemon received SIGTERM, shutting down")?;
                break;
            }
        }
    }
    let _ = fs::remove_file(config_dir()?.join("sync.pid"));
    Ok(())
}

/// Resolves when the process receives SIGTERM (unix); never resolves elsewhere.
async fn terminate_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    }
    #[cfg(not(unix))]
    {
        std::future::pending::<()>().await
    }
}
