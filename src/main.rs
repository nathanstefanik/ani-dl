//! ani-dl — a zero-runtime-dependency Rust anime downloader. No playback, ever.

mod api;
mod cli;
mod config;
mod constants;
mod hls;
mod minter;
mod providers;
mod sync;
mod tui;

use std::path::PathBuf;
use std::sync::LazyLock;

use anyhow::Result;
use clap::Parser;
use regex::Regex;

use api::{AllAnimeClient, ShowResult, TranslationType};
use cli::{Cli, Command};
use config::Config;
use hls::HlsDownloader;
use providers::{resolve_all, select_quality};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut cfg = Config::load().unwrap_or_else(|e| {
        eprintln!("config error: {e:#} — using defaults");
        Config::default()
    });
    if !cfg.api.allanime_key.is_empty() {
        constants::set_active_key(cfg.api.allanime_key.clone());
    }

    match &cli.command {
        Some(Command::Sync { daemon }) => {
            if *daemon {
                sync::run_daemon(cfg).await
            } else {
                sync::run_once(&mut cfg, true).await
            }
        }
        Some(Command::Config) => {
            let path = config::config_path()?;
            println!("Config path: {}", path.display());
            println!("---\n{}", toml::to_string_pretty(&cfg)?);
            Ok(())
        }
        Some(Command::Hls {
            url,
            out,
            concurrency,
            quality,
            referer,
        }) => {
            let client = api::download_client()?;
            let dl = HlsDownloader::new(client, *concurrency, referer.clone(), 3);
            let path = dl.download(url, std::path::Path::new(out), quality).await?;
            println!("Downloaded: {}", path.display());
            Ok(())
        }
        None => run_download(&cli, &cfg).await,
    }
}

async fn run_download(cli: &Cli, cfg: &Config) -> Result<()> {
    let mode = if cli.dubbed {
        TranslationType::Dub
    } else {
        TranslationType::Sub
    };
    let concurrency = cli.concurrency.unwrap_or(cfg.download.concurrency);
    let quality = cli
        .quality
        .clone()
        .unwrap_or_else(|| cfg.download.quality.clone());
    let out_dir = PathBuf::from(
        cli.download_dir
            .clone()
            .unwrap_or_else(|| cfg.download.directory.clone()),
    );

    let api = AllAnimeClient::new(cfg).await?;

    // Determine query.
    let query = match &cli.query {
        Some(q) => q.clone(),
        None => {
            if cli.no_tui {
                anyhow::bail!("--no-tui requires a QUERY argument");
            }
            prompt("Search anime: ")?
        }
    };
    if query.trim().is_empty() {
        anyhow::bail!("empty query");
    }

    eprintln!("Searching '{}' ({})...", query, mode.as_str());
    let results = api.search(&query, mode).await?;
    if results.is_empty() {
        anyhow::bail!("no results found");
    }

    // Select show.
    let show = pick_show(&results, cli)?;
    let Some(show) = show else {
        return Ok(());
    };
    let season = match cli.season {
        Some(n) => n,
        None => infer_season(&show, &results, &api, mode).await,
    };
    eprintln!(
        "Selected: {} ({} eps, season {season})",
        show.name, show.episodes
    );

    // Episode list.
    let available = api.episode_list(&show.id, mode).await?;
    if available.is_empty() {
        anyhow::bail!("no episodes available for this translation type");
    }

    // Select episodes.
    let episodes = pick_episodes(&available, cli)?;
    if episodes.is_empty() {
        eprintln!("No episodes selected.");
        return Ok(());
    }

    let dl_client = api::download_client()?;

    let mut failures = 0;
    let mut skipped = 0;
    let total = episodes.len();
    for (i, ep) in episodes.iter().enumerate() {
        eprintln!("\n=== Episode {ep} ({}/{total}) ===", i + 1);
        // Keep the loop failure-tolerant: one bad episode must not abort the batch.
        let sources = match api.episode_sources(&show.id, ep, mode).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("  ! failed to fetch sources for episode {ep}: {e:#}");
                failures += 1;
                continue;
            }
        };
        let names: Vec<&str> = sources.iter().map(|s| s.source_name.as_str()).collect();
        eprintln!("  providers: {}", if names.is_empty() { "(none)".to_string() } else { names.join(", ") });
        let streams = resolve_all(&api.client, &sources).await;
        eprintln!("  resolved {} stream(s): {}", streams.len(), summarize_streams(&streams));

        if cli.list_providers {
            if streams.is_empty() {
                eprintln!("  (no streams resolved)");
            }
            let mut ordered: Vec<&providers::Stream> = streams.iter().collect();
            ordered.sort_by_key(|s| std::cmp::Reverse(s.height));
            for s in &ordered {
                println!("  [{:>10}] {:>5}  {}", s.provider, fmt_height(s.height), s.url);
            }
            continue;
        }

        let Some(chosen) = select_quality(&streams, &quality) else {
            eprintln!("  ! no stream found for episode {ep}");
            failures += 1;
            continue;
        };
        eprintln!("  source: {} ({})", chosen.provider, fmt_height(chosen.height));
        eprintln!("  url: {}", chosen.url);

        std::fs::create_dir_all(&out_dir)?;
        let filename = build_filename(&show.name, season, ep);
        let out_path = out_dir.join(format!("{filename}.mp4"));

        if out_path.exists() && !cli.force {
            let size = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
            eprintln!(
                "  skipping: {} already exists ({})",
                out_path.display(),
                indicatif::HumanBytes(size),
            );
            skipped += 1;
            continue;
        }

        eprintln!("  saving to: {}", out_path.display());

        let dl = HlsDownloader::new(
            dl_client.clone(),
            concurrency,
            chosen.referer.clone(),
            cfg.download.retries,
        );
        let started = std::time::Instant::now();
        match dl.download(&chosen.url, &out_path, &quality).await {
            Ok(path) => {
                let secs = started.elapsed().as_secs_f64();
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                println!(
                    "Downloaded: {} ({} in {:.0}s, {}/s avg)",
                    path.display(),
                    indicatif::HumanBytes(size),
                    secs,
                    indicatif::HumanBytes((size as f64 / secs.max(0.001)) as u64),
                );
            }
            Err(e) => {
                // {:#} prints the whole anyhow chain — the top-level reqwest
                // message alone ("error decoding response body") hides the cause.
                eprintln!("  ! download failed for episode {ep}: {e:#}");
                failures += 1;
            }
        }
    }

    if failures > 0 {
        anyhow::bail!("{failures} episode(s) failed");
    }
    if skipped > 0 {
        eprintln!("\nSkipped {skipped} existing file(s) (use -f/--force to re-download).");
    }
    Ok(())
}

/// "1080p", or "???" when the height is unknown (0).
fn fmt_height(h: u32) -> String {
    if h > 0 { format!("{h}p") } else { "???".to_string() }
}

/// "yt (1080p), hls (720p), mp4upload (???)" — one entry per resolved stream.
fn summarize_streams(streams: &[providers::Stream]) -> String {
    if streams.is_empty() {
        return "(none)".to_string();
    }
    streams
        .iter()
        .map(|s| format!("{} ({})", s.provider, fmt_height(s.height)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn pick_show(results: &[ShowResult], cli: &Cli) -> Result<Option<ShowResult>> {
    if let Some(n) = cli.number {
        if n >= 1 && n <= results.len() {
            return Ok(Some(results[n - 1].clone()));
        }
        anyhow::bail!("--number {n} out of range (1-{})", results.len());
    }
    if cli.no_tui {
        for (i, s) in results.iter().enumerate() {
            let year = if s.year > 0 { format!(" ({})", s.year) } else { String::new() };
            println!("{}\t{} ({} episodes){year}", i + 1, s.name, s.episodes);
        }
        anyhow::bail!("--no-tui: re-run with -n <N> to pick a result");
    }
    tui::select_show(results.to_vec())
}

static RE_SEASON_NUM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)season\s*(\d+)").unwrap());
static RE_ORDINAL_SEASON: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(\d+)(?:st|nd|rd|th)\s+season").unwrap());

/// Infer Sxx from the show title and sibling search hits (e.g. Kaguya S1 `:` vs S2 `?`).
async fn infer_season(
    show: &ShowResult,
    search_results: &[ShowResult],
    api: &AllAnimeClient,
    mode: TranslationType,
) -> u32 {
    if let Some(n) = explicit_season_in_title(&show.name) {
        return n;
    }
    if let Some(n) = season_by_franchise_rank(show, search_results) {
        return n;
    }
    let prefix = franchise_key(&show.name);
    if prefix.len() >= 8 {
        if let Ok(more) = api.search(&prefix, mode).await {
            if let Some(n) = season_by_franchise_rank(show, &more) {
                return n;
            }
        }
    }
    1
}

fn explicit_season_in_title(name: &str) -> Option<u32> {
    RE_SEASON_NUM
        .captures(name)
        .or_else(|| RE_ORDINAL_SEASON.captures(name))
        .and_then(|c| c[1].parse().ok())
        .filter(|&n| n > 0)
}

fn franchise_key(name: &str) -> String {
    let lower = name.to_lowercase();
    let base = lower
        .split(": ")
        .next()
        .or_else(|| lower.split("? ").next())
        .unwrap_or(&lower);
    RE_SEASON_NUM
        .replace(base, "")
        .trim()
        .to_string()
}

fn is_special_entry(name: &str, episodes: u32) -> bool {
    if episodes <= 5 {
        return true;
    }
    let lower = name.to_lowercase();
    [
        " ova",
        " movie",
        "picture drama",
        "teaser",
        " gaiden",
        "kanketsu-hen",
        "chuugakkou",
        "spinoff",
    ]
    .iter()
    .any(|kw| lower.contains(kw))
}

fn season_by_franchise_rank(show: &ShowResult, search_results: &[ShowResult]) -> Option<u32> {
    let key = franchise_key(&show.name);
    let mut cousins: Vec<&ShowResult> = search_results
        .iter()
        .filter(|s| franchise_key(&s.name) == key)
        .filter(|s| !is_special_entry(&s.name, s.episodes))
        .collect();
    if cousins.len() <= 1 {
        return None;
    }
    cousins.sort_by(|a, b| {
        a.year
            .cmp(&b.year)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
    cousins
        .iter()
        .position(|s| s.id == show.id)
        .map(|i| (i + 1) as u32)
}

fn pick_episodes(available: &[String], cli: &Cli) -> Result<Vec<String>> {
    if let Some(arg) = &cli.episodes {
        return Ok(parse_episode_arg(arg, available));
    }
    if cli.no_tui {
        anyhow::bail!("--no-tui requires -e <RANGE> to pick episodes");
    }
    // `available` is already sorted by api::episode_list and the TUI preserves
    // its order, so no re-sort is needed.
    tui::select_episodes(available.to_vec())
}

/// Expand "1", "1-12", "1 2 5", "1,3,5" against the available episode list.
fn parse_episode_arg(arg: &str, available: &[String]) -> Vec<String> {
    let avail: std::collections::HashSet<&str> = available.iter().map(|s| s.as_str()).collect();
    let mut picked: Vec<String> = Vec::new();
    for token in arg.split(|c: char| c == ',' || c.is_whitespace()) {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if let Some((lo, hi)) = token.split_once('-') {
            if let (Ok(lo), Ok(hi)) = (lo.parse::<u32>(), hi.parse::<u32>()) {
                // Walk the available list rather than lo..=hi, so a typo like
                // "1-9999999" cannot expand into millions of strings.
                picked.extend(
                    available
                        .iter()
                        .filter(|ep| ep.parse::<u32>().is_ok_and(|n| (lo..=hi).contains(&n)))
                        .cloned(),
                );
                continue;
            }
        }
        picked.push(token.to_string());
    }
    let mut result = Vec::new();
    for ep in picked {
        if avail.contains(ep.as_str()) {
            if !result.contains(&ep) {
                result.push(ep);
            }
        } else {
            eprintln!("  ! episode {ep} not available, skipping");
        }
    }
    result
}

static RE_UNSAFE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\w\s-]").unwrap());
static RE_WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\s_]+").unwrap());

/// "Tongari.Boushi.no.Atelier.S01E13" (extension added by the downloader).
fn build_filename(name: &str, season: u32, episode: &str) -> String {
    let cleaned = RE_UNSAFE.replace_all(name, "");
    let dotted = RE_WS.replace_all(cleaned.trim(), ".");
    let dotted = dotted.trim_matches('.');
    let title = if dotted.is_empty() { "anime" } else { dotted };

    let ep_tag = match episode.parse::<f64>() {
        Ok(f) if f.fract() == 0.0 => format!("E{:02}", f as u64),
        _ => format!("E{episode}"),
    };
    format!("{title}.S{season:02}{ep_tag}")
}

fn prompt(msg: &str) -> Result<String> {
    use std::io::{self, Write};
    print!("{msg}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}
