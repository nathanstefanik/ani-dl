//! Per-episode download: stream resolution, filename building, HLS download
//! and subtitle sidecars.

use std::path::Path;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;

use crate::api::{HianimeClient, TranslationType};
use crate::hls::HlsDownloader;
use crate::providers::{select_quality, Stream};

/// "1080p", or "???" when the height is unknown (0).
pub(crate) fn fmt_height(h: u32) -> String {
    if h > 0 {
        format!("{h}p")
    } else {
        "???".to_string()
    }
}

/// "yt (1080p), hls (720p), mp4upload (???)" — one entry per resolved stream.
pub(crate) fn summarize_streams(streams: &[Stream]) -> String {
    if streams.is_empty() {
        return "(none)".to_string();
    }
    streams
        .iter()
        .map(|s| format!("{} ({})", s.provider, fmt_height(s.height)))
        .collect::<Vec<_>>()
        .join(", ")
}

static RE_UNSAFE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\w\s-]").unwrap());
static RE_UNSAFE_EP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\w.-]").unwrap());
static RE_WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\s_]+").unwrap());

/// "Tongari.Boushi.no.Atelier.S01E13" (extension added by the downloader).
pub(crate) fn build_filename(name: &str, season: u32, episode: &str) -> String {
    let cleaned = RE_UNSAFE.replace_all(name, "");
    let dotted = RE_WS.replace_all(cleaned.trim(), ".");
    let dotted = dotted.trim_matches('.');
    let title = if dotted.is_empty() { "anime" } else { dotted };

    let ep_tag = match episode.parse::<f64>() {
        Ok(f) if f.fract() == 0.0 => format!("E{:02}", f as u64),
        _ => format!("E{}", RE_UNSAFE_EP.replace_all(episode, "")),
    };
    format!("{title}.S{season:02}{ep_tag}")
}

/// Everything a single-episode download needs that doesn't come from the
/// episode number itself.
pub struct EpisodeTarget<'a> {
    pub show_id: &'a str,
    pub file_title: &'a str,
    pub season: u32,
    pub mode: TranslationType,
    pub quality: &'a str,
    pub out_dir: &'a Path,
    pub concurrency: usize,
    pub retries: u32,
    pub force: bool,
}

pub enum EpisodeOutcome {
    Downloaded,
    Skipped,
}

/// Resolve, download and subtitle one episode. Prints progress lines;
/// failures are returned as `Err` for the caller to report and count.
pub async fn download_episode(
    api: &HianimeClient,
    dl_client: &wreq::Client,
    t: &EpisodeTarget<'_>,
    ep: &str,
) -> Result<EpisodeOutcome> {
    let streams = api
        .episode_streams(t.show_id, ep, t.mode)
        .await
        .with_context(|| format!("failed to resolve episode {ep}"))?;
    eprintln!(
        "  resolved {} stream(s): {}",
        streams.len(),
        summarize_streams(&streams)
    );

    let Some(chosen) = select_quality(&streams, t.quality) else {
        anyhow::bail!("no stream found for episode {ep}");
    };
    eprintln!(
        "  source: {} ({})",
        chosen.provider,
        fmt_height(chosen.height)
    );
    eprintln!("  url: {}", chosen.url);

    std::fs::create_dir_all(t.out_dir)?;
    let filename = build_filename(t.file_title, t.season, ep);
    let out_path = t.out_dir.join(format!("{filename}.mp4"));

    if out_path.exists() && !t.force {
        let size = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
        eprintln!(
            "  skipping: {} already exists ({})",
            out_path.display(),
            indicatif::HumanBytes(size),
        );
        return Ok(EpisodeOutcome::Skipped);
    }

    eprintln!("  saving to: {}", out_path.display());

    let dl = HlsDownloader::new(
        dl_client.clone(),
        t.concurrency,
        chosen.referer.clone(),
        t.retries,
    );
    let started = std::time::Instant::now();
    let path = dl
        .download(&chosen.url, &out_path, t.quality)
        .await
        .with_context(|| format!("download failed for episode {ep}"))?;
    let secs = started.elapsed().as_secs_f64();
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    println!(
        "Downloaded: {} ({} in {:.0}s, {}/s avg)",
        path.display(),
        indicatif::HumanBytes(size),
        secs,
        indicatif::HumanBytes((size as f64 / secs.max(0.001)) as u64),
    );
    if let Some(sub_url) = &chosen.subtitle {
        let vtt_path = out_path.with_extension("vtt");
        match download_sidecar(dl_client, sub_url, &chosen.referer, &vtt_path).await {
            Ok(()) => eprintln!("  subtitles: {}", vtt_path.display()),
            Err(e) => eprintln!("  ! subtitle download failed: {e:#}"),
        }
    }
    Ok(EpisodeOutcome::Downloaded)
}

async fn download_sidecar(
    client: &wreq::Client,
    url: &str,
    referer: &str,
    path: &std::path::Path,
) -> Result<()> {
    let bytes = client
        .get(url)
        .header("Referer", referer)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    check_vtt(&bytes)?;
    tokio::fs::write(path, &bytes).await?;
    Ok(())
}

/// A `.vtt` sidecar must actually be WebVTT — a subtitle URL that quietly
/// serves an HTML error page with HTTP 200 is skipped instead of written.
fn check_vtt(body: &[u8]) -> Result<()> {
    let body = body.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(body);
    if body.starts_with(b"WEBVTT") {
        return Ok(());
    }
    anyhow::bail!(
        "not a WebVTT file (starts with {:?})",
        String::from_utf8_lossy(&body[..body.len().min(16)])
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_uses_base_title_for_known_season() {
        let base =
            crate::base_title("Kaguya-sama wa Kokurasetai? Tensai-tachi no Renai Zunousen");
        assert_eq!(
            build_filename(&base, 2, "1"),
            "Kaguya-sama.wa.Kokurasetai.S02E01"
        );
    }

    #[test]
    fn filename_strips_windows_unsafe_chars() {
        let name = build_filename("Fate/Zero: <Test> \"A|B\" ?*\\", 1, "1");
        for c in "<>:\"/\\|?*".chars() {
            assert!(!name.contains(c), "filename contains {c:?}: {name}");
        }
        assert_eq!(name, "FateZero.Test.AB.S01E01");
    }

    #[test]
    fn check_vtt_wants_a_webvtt_header() {
        assert!(check_vtt(b"WEBVTT\n\n00:01.000 --> 00:02.000\nhi\n").is_ok());
        assert!(check_vtt(b"\xEF\xBB\xBFWEBVTT\n").is_ok());
        assert!(check_vtt(b"<html><body><h1>404 Not Found</h1></body></html>").is_err());
        assert!(check_vtt(b"").is_err());
    }

    #[test]
    fn filename_episode_tag_is_sanitized() {
        assert_eq!(build_filename("Show", 1, "12.5"), "Show.S01E12.5");
        assert_eq!(build_filename("Show", 1, "1/2"), "Show.S01E12");
        assert!(!build_filename("Show", 1, "1/2").contains('/'));
        assert_eq!(build_filename("Show", 2, "3"), "Show.S02E03");
    }
}
