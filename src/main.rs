//! ani-dl — a zero-runtime-dependency Rust anime downloader. No playback, ever.

mod api;
mod cli;
mod config;
mod constants;
mod hls;
mod providers;
mod sync;
mod tui;
mod version;

use std::path::PathBuf;
use std::sync::LazyLock;

use anyhow::Result;
use clap::Parser;
use regex::Regex;

use api::{AnidbClient, ShowResult, TranslationType};
use cli::{Cli, Command};
use config::Config;
use hls::HlsDownloader;
use providers::select_quality;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load().unwrap_or_else(|e| {
        eprintln!("config error: {e:#} — using defaults");
        Config::default()
    });

    match &cli.command {
        Some(Command::Sync { daemon }) => {
            if *daemon {
                sync::run_daemon(cfg).await
            } else {
                sync::run_once(&cfg, true).await
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

    let api = AnidbClient::new()?;

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
    let known_season = match cli.season {
        Some(n) => Some(n),
        None => infer_season(&show, &results, &api, mode).await,
    };
    let season = known_season.unwrap_or(1);
    // Once the season is known, the subtitle is redundant — S02 already says
    // it — so name files after the franchise base ("Kaguya-sama wa
    // Kokurasetai" instead of "... ? Tensai-tachi no Renai Zunousen").
    let file_title = match known_season {
        Some(_) => {
            let base = base_title(&show.name);
            if base.is_empty() {
                show.name.clone()
            } else {
                base
            }
        }
        None => show.name.clone(),
    };

    // Episode list.
    let available = api.episode_list(&show.id, mode).await?;
    if available.is_empty() {
        anyhow::bail!("no episodes available");
    }
    eprintln!(
        "Selected: {} ({} eps, season {season})",
        show.name,
        available.len()
    );

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
        let streams = match api.episode_streams(&show.id, ep, mode).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("  ! failed to resolve episode {ep}: {e:#}");
                failures += 1;
                continue;
            }
        };
        eprintln!(
            "  resolved {} stream(s): {}",
            streams.len(),
            summarize_streams(&streams)
        );

        if cli.list_providers {
            if streams.is_empty() {
                eprintln!("  (no streams resolved)");
            }
            let mut ordered: Vec<&providers::Stream> = streams.iter().collect();
            ordered.sort_by_key(|s| std::cmp::Reverse(s.height));
            for s in &ordered {
                println!(
                    "  [{:>10}] {:>5}  {}",
                    s.provider,
                    fmt_height(s.height),
                    s.url
                );
            }
            continue;
        }

        let Some(chosen) = select_quality(&streams, &quality) else {
            eprintln!("  ! no stream found for episode {ep}");
            failures += 1;
            continue;
        };
        eprintln!(
            "  source: {} ({})",
            chosen.provider,
            fmt_height(chosen.height)
        );
        eprintln!("  url: {}", chosen.url);

        std::fs::create_dir_all(&out_dir)?;
        let filename = build_filename(&file_title, season, ep);
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
    if h > 0 {
        format!("{h}p")
    } else {
        "???".to_string()
    }
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
            let year = if s.year > 0 {
                format!(" ({})", s.year)
            } else {
                String::new()
            };
            let eps = if s.episodes > 0 {
                format!(" ({} episodes)", s.episodes)
            } else {
                String::new()
            };
            println!("{}\t{}{eps}{year}", i + 1, s.name);
        }
        anyhow::bail!("--no-tui: re-run with -n <N> to pick a result");
    }
    tui::select_show(results.to_vec())
}

static RE_SEASON_NUM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)season\s*(\d+)").unwrap());
static RE_ORDINAL_SEASON: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(\d+)(?:st|nd|rd|th)\s+season").unwrap());

/// Infer Sxx from the show title and sibling search hits (e.g. Kaguya S1 `:`
/// vs S2 `?`). `None` means no evidence either way — caller defaults to 1.
async fn infer_season(
    show: &ShowResult,
    search_results: &[ShowResult],
    api: &AnidbClient,
    mode: TranslationType,
) -> Option<u32> {
    if let Some(n) = explicit_season_in_title(&show.name) {
        return Some(n);
    }
    if let Some(n) = season_by_franchise_rank(show, search_results) {
        return Some(n);
    }
    let prefix = franchise_key(&show.name);
    if prefix.len() >= 8
        && let Ok(more) = api.search(&prefix, mode).await
        && let Some(n) = season_by_franchise_rank(show, &more)
    {
        return Some(n);
    }
    None
}

fn explicit_season_in_title(name: &str) -> Option<u32> {
    RE_SEASON_NUM
        .captures(name)
        .or_else(|| RE_ORDINAL_SEASON.captures(name))
        .and_then(|c| c[1].parse().ok())
        .filter(|&n| n > 0)
}

/// Original-case franchise base of a title: cut at the first subtitle
/// separator and drop explicit season markers ("Season 2", "2nd Season").
fn base_title(name: &str) -> String {
    let cut = [": ", "? ", "! "]
        .iter()
        .filter_map(|sep| name.find(sep))
        .min()
        .unwrap_or(name.len());
    let base = RE_SEASON_NUM.replace(&name[..cut], "");
    let base = RE_ORDINAL_SEASON.replace(&base, "");
    base.trim_end_matches(['?', '!', ':', '.', '-', ' '])
        .trim()
        .to_string()
}

fn franchise_key(name: &str) -> String {
    let lower = name.to_lowercase();
    // Cut at the earliest subtitle separator so titles that differ only in
    // that punctuation (Kaguya S1 ": " vs S2 "? ") collapse to the same key.
    let cut = [": ", "? ", "! "]
        .iter()
        .filter_map(|sep| lower.find(sep))
        .min()
        .unwrap_or(lower.len());
    RE_SEASON_NUM
        .replace(&lower[..cut], "")
        .trim_end_matches(['?', '!', ':', '.'])
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
///
/// Range bounds follow ani-cli 5.0.4: `0` as the start means the first
/// available episode and `-1` means the last, so "0--1" is the whole show and
/// "12--1" is episode 12 onwards. `-1` on its own is the latest episode.
fn parse_episode_arg(arg: &str, available: &[String]) -> Vec<String> {
    let avail: std::collections::HashSet<&str> = available.iter().map(|s| s.as_str()).collect();
    let mut picked: Vec<String> = Vec::new();
    for token in arg.split(|c: char| c == ',' || c.is_whitespace()) {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if token == "-1" {
            picked.extend(available.last().cloned());
            continue;
        }
        if let Some((lo_raw, hi_raw)) = split_range(token)
            && let (Some(lo), Some(hi)) = (
                range_bound(lo_raw, available, Bound::Start),
                range_bound(hi_raw, available, Bound::End),
            )
        {
            // Walk the available list rather than lo..=hi, so a typo like
            // "1-9999999" cannot expand into millions of strings.
            let matched: Vec<String> = available
                .iter()
                .filter(|ep| ep.parse::<f64>().is_ok_and(|n| n >= lo && n <= hi))
                .cloned()
                .collect();
            if matched.is_empty() {
                eprintln!("  ! range {token} matches no available episode, skipping");
            }
            picked.extend(matched);
            continue;
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

/// Split "1-12" / "12--1" into its two bounds. The separator is the first `-`
/// after position 0, so a leading "-1" stays intact as a bound of its own.
fn split_range(token: &str) -> Option<(&str, &str)> {
    let after_first = token.chars().next()?.len_utf8();
    let sep = token[after_first..].find('-')? + after_first;
    Some((&token[..sep], &token[sep + 1..]))
}

enum Bound {
    Start,
    End,
}

/// Resolve one range bound to a number: `0` is the first available episode,
/// `-1` the last, anything else is parsed as written.
fn range_bound(raw: &str, available: &[String], side: Bound) -> Option<f64> {
    let edge = |ep: Option<&String>| ep.and_then(|e| e.parse::<f64>().ok());
    match raw {
        "-1" => edge(available.last()),
        "0" if matches!(side, Bound::Start) => edge(available.first()),
        other => other.parse::<f64>().ok(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn show(id: &str, name: &str, episodes: u32, year: u32) -> ShowResult {
        ShowResult {
            id: id.to_string(),
            name: name.to_string(),
            episodes,
            year,
        }
    }

    #[test]
    fn franchise_key_collapses_separator_punctuation() {
        // Kaguya S1 uses ": ", S2 uses "? " — both must map to the same key.
        let k1 = franchise_key("Kaguya-sama wa Kokurasetai: Tensai-tachi no Renai Zunousen");
        let k2 = franchise_key("Kaguya-sama wa Kokurasetai? Tensai-tachi no Renai Zunousen");
        let k3 = franchise_key("Kaguya-sama wa Kokurasetai: Ultra Romantic");
        assert_eq!(k1, "kaguya-sama wa kokurasetai");
        assert_eq!(k1, k2);
        assert_eq!(k1, k3);
    }

    #[test]
    fn franchise_key_strips_trailing_punctuation_and_season_markers() {
        assert_eq!(franchise_key("Haikyuu!!"), "haikyuu");
        assert_eq!(franchise_key("Haikyuu!! 2nd Season"), "haikyuu");
        assert_eq!(franchise_key("Bocchi the Rock! 2"), "bocchi the rock");
        assert_eq!(franchise_key("Dr. STONE Season 2"), "dr. stone");
    }

    #[test]
    fn kaguya_season_two_ranks_second() {
        let s1 = show(
            "a",
            "Kaguya-sama wa Kokurasetai: Tensai-tachi no Renai Zunousen",
            12,
            2019,
        );
        let s2 = show(
            "b",
            "Kaguya-sama wa Kokurasetai? Tensai-tachi no Renai Zunousen",
            12,
            2020,
        );
        let s3 = show("c", "Kaguya-sama wa Kokurasetai: Ultra Romantic", 13, 2022);
        let ova = show(
            "d",
            "Kaguya-sama wa Kokurasetai: Tensai-tachi no Renai Zunousen OVA",
            1,
            2021,
        );
        let results = vec![s1.clone(), s2.clone(), s3.clone(), ova];
        assert_eq!(season_by_franchise_rank(&s1, &results), Some(1));
        assert_eq!(season_by_franchise_rank(&s2, &results), Some(2));
        assert_eq!(season_by_franchise_rank(&s3, &results), Some(3));
    }

    #[test]
    fn lone_result_gives_no_rank() {
        let s1 = show("a", "Sousou no Frieren", 28, 2023);
        assert_eq!(
            season_by_franchise_rank(&s1, std::slice::from_ref(&s1)),
            None
        );
    }

    #[test]
    fn base_title_keeps_case_and_drops_subtitle() {
        assert_eq!(
            base_title("Kaguya-sama wa Kokurasetai? Tensai-tachi no Renai Zunousen"),
            "Kaguya-sama wa Kokurasetai"
        );
        assert_eq!(
            base_title("Kaguya-sama wa Kokurasetai: Ultra Romantic"),
            "Kaguya-sama wa Kokurasetai"
        );
        assert_eq!(base_title("Mob Psycho 100 Season 3"), "Mob Psycho 100");
        assert_eq!(base_title("Haikyuu!! 2nd Season"), "Haikyuu");
        assert_eq!(base_title("Sousou no Frieren"), "Sousou no Frieren");
    }

    #[test]
    fn filename_uses_base_title_for_known_season() {
        let base = base_title("Kaguya-sama wa Kokurasetai? Tensai-tachi no Renai Zunousen");
        assert_eq!(
            build_filename(&base, 2, "1"),
            "Kaguya-sama.wa.Kokurasetai.S02E01"
        );
    }

    #[test]
    fn range_bounds_follow_ani_cli() {
        let eps: Vec<String> = ["2", "3", "4", "5"].iter().map(|s| s.to_string()).collect();
        // "0" as a start and "-1" as an end are the first/last available episode.
        assert_eq!(parse_episode_arg("0--1", &eps), eps);
        assert_eq!(parse_episode_arg("3--1", &eps), ["3", "4", "5"]);
        assert_eq!(parse_episode_arg("0-3", &eps), ["2", "3"]);
        // "-1" alone is the latest episode.
        assert_eq!(parse_episode_arg("-1", &eps), ["5"]);
        // Ordinary forms are unchanged.
        assert_eq!(parse_episode_arg("3-4", &eps), ["3", "4"]);
        assert_eq!(parse_episode_arg("5 3", &eps), ["5", "3"]);
        assert_eq!(parse_episode_arg("2,4", &eps), ["2", "4"]);
        assert!(parse_episode_arg("8-9", &eps).is_empty());
    }

    #[test]
    fn odd_tokens_are_passed_through_not_panicked_on() {
        let eps: Vec<String> = ["1", "2"].iter().map(|s| s.to_string()).collect();
        // Multi-byte first character: must not byte-slice mid-character.
        assert!(parse_episode_arg("ワンピース-1", &eps).is_empty());
        assert!(parse_episode_arg("-", &eps).is_empty());
        assert!(parse_episode_arg("a-b", &eps).is_empty());
    }

    #[test]
    fn range_matches_decimal_episodes() {
        let eps: Vec<String> = ["1", "1.5", "2"].iter().map(|s| s.to_string()).collect();
        assert_eq!(parse_episode_arg("1-2", &eps), ["1", "1.5", "2"]);
        assert_eq!(parse_episode_arg("1.5", &eps), ["1.5"]);
    }

    #[test]
    fn explicit_markers_win() {
        assert_eq!(explicit_season_in_title("Mob Psycho 100 Season 3"), Some(3));
        assert_eq!(explicit_season_in_title("Haikyuu!! 2nd Season"), Some(2));
        assert_eq!(
            explicit_season_in_title("Kaguya-sama wa Kokurasetai? Tensai-tachi no Renai Zunousen"),
            None
        );
    }
}
