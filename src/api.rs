//! anidb.app client (ani-cli v5 provider). Uses wreq Chrome TLS emulation to
//! clear Cloudflare; plain reqwest gets 403.

use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use regex::Regex;
use serde::Deserialize;
use wreq::header::{HeaderMap, HeaderValue, USER_AGENT as H_UA};
use wreq_util::Emulation;

use crate::constants::{ANIDB_BASE, ANIDB_REFERER, USER_AGENT};
use crate::providers::Stream;

static RE_SEARCH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"anime/([^"]+-[0-9]+)"[^>]*title="([^"]+)""#).expect("search regex")
});
static RE_EMBED_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"file:\s*'([^']+)'").expect("embed file regex"));
static RE_RESOLUTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"RESOLUTION=\d+x(\d+)").expect("resolution regex"));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslationType {
    Sub,
    Dub,
}

impl TranslationType {
    pub fn as_str(self) -> &'static str {
        match self {
            TranslationType::Sub => "sub",
            TranslationType::Dub => "dub",
        }
    }

    /// anidb language code: Japanese softsubs vs English dub.
    fn lang_code(self) -> &'static str {
        match self {
            TranslationType::Sub => "jpn",
            TranslationType::Dub => "eng",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ShowResult {
    /// Slug id, e.g. `cyberpunk-edgerunners-1118`.
    pub id: String,
    pub name: String,
    /// Filled after `episode_list`; search cards do not expose a count.
    pub episodes: u32,
    /// Not available from browse cards; always 0 for now.
    pub year: u32,
}

pub struct AnidbClient {
    pub client: wreq::Client,
}

fn default_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(H_UA, HeaderValue::from_static(USER_AGENT));
    headers
}

/// Shared wreq client with Chrome TLS fingerprint. Used for both API and
/// downloads — anidb.app (and its HLS CDN) sit behind Cloudflare.
pub fn http_client() -> Result<wreq::Client> {
    wreq::Client::builder()
        .emulation(Emulation::Chrome136)
        .default_headers(default_headers())
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .build()
        .context("building wreq client")
}

/// Same fingerprint, but no whole-request timeout — HLS segment bodies can
/// take longer than a fixed deadline on a slow link.
pub fn download_client() -> Result<wreq::Client> {
    wreq::Client::builder()
        .emulation(Emulation::Chrome136)
        .default_headers(default_headers())
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .build()
        .context("building download client")
}

impl AnidbClient {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: http_client()?,
        })
    }

    async fn get_text(&self, url: &str) -> Result<String> {
        self.client
            .get(url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("GET {url} status"))?
            .text()
            .await
            .with_context(|| format!("GET {url} body"))
    }

    pub async fn search(&self, query: &str, _mode: TranslationType) -> Result<Vec<ShowResult>> {
        let q = query.trim().replace(' ', "+");
        let url = format!("{ANIDB_BASE}/browse?q={q}");
        let html = self.get_text(&url).await?;
        if html.contains("Just a moment...") || html.contains("cf-mitigated") {
            anyhow::bail!(
                "blocked by Cloudflare on search; TLS fingerprint may need updating (wreq Emulation)"
            );
        }

        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for cap in RE_SEARCH.captures_iter(&html) {
            let id = cap[1].to_string();
            if !seen.insert(id.clone()) {
                continue;
            }
            out.push(ShowResult {
                id,
                name: html_unescape(&cap[2]),
                episodes: 0,
                year: 0,
            });
        }
        Ok(out)
    }

    /// Episode numbers as strings (`"1"`, `"2"`, …), sorted ascending.
    pub async fn episode_list(&self, show_id: &str, _mode: TranslationType) -> Result<Vec<String>> {
        let maps = self.episode_maps(show_id).await?;
        Ok(maps.into_iter().map(|(_, num)| num).collect())
    }

    /// `(episode_id, episode_number)` pairs for a show slug.
    async fn episode_maps(&self, show_id: &str) -> Result<Vec<(u64, String)>> {
        let numeric = show_numeric_id(show_id)
            .ok_or_else(|| anyhow!("invalid show id (expected slug-N): {show_id}"))?;
        let url = format!("{ANIDB_BASE}/api/frontend/anime/{numeric}/episodes");
        let text = self.get_text(&url).await?;
        let parsed: EpisodesResponse =
            serde_json::from_str(&text).context("parsing episodes JSON")?;
        let mut maps: Vec<(u64, String)> = parsed
            .episodes
            .into_iter()
            .map(|e| (e.id, e.number.to_string()))
            .collect();
        maps.sort_by(|a, b| {
            ep_sort_key(&a.1)
                .partial_cmp(&ep_sort_key(&b.1))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(maps)
    }

    /// Resolve playable HLS variants for one episode.
    pub async fn episode_streams(
        &self,
        show_id: &str,
        ep: &str,
        mode: TranslationType,
    ) -> Result<Vec<Stream>> {
        let maps = self.episode_maps(show_id).await?;
        let ep_id = maps
            .iter()
            .find(|(_, num)| num == ep)
            .map(|(id, _)| *id)
            .ok_or_else(|| anyhow!("episode {ep} not in list for {show_id}"))?;

        let url = format!("{ANIDB_BASE}/api/frontend/episode/{ep_id}/languages");
        let text = self.get_text(&url).await?;
        let parsed: LanguagesResponse =
            serde_json::from_str(&text).context("parsing languages JSON")?;

        let want = mode.lang_code();
        let embed = parsed
            .languages
            .iter()
            .find(|l| l.code == want)
            .or_else(|| parsed.languages.first())
            .map(|l| l.embed_url.replace("\\/", "/"))
            .ok_or_else(|| anyhow!("no language embeds for episode {ep}"))?;

        if !parsed.languages.iter().any(|l| l.code == want) {
            eprintln!(
                "  ! {} not available, using {}",
                mode.as_str(),
                parsed.languages[0].code
            );
        }

        let embed_html = self.get_text(&embed).await?;
        let master = RE_EMBED_FILE
            .captures(&embed_html)
            .map(|c| c[1].to_string())
            .ok_or_else(|| anyhow!("no m3u8 file: in embed page"))?;

        self.expand_master(&master).await
    }

    async fn expand_master(&self, master_url: &str) -> Result<Vec<Stream>> {
        let text = self.get_text(master_url).await?;
        if !text.contains("EXTM3U") {
            anyhow::bail!("master playlist is not m3u8");
        }
        let lines: Vec<&str> = text.lines().collect();
        let mut streams = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if !line.starts_with("#EXT-X-STREAM-INF") || line.contains("EXT-X-I-FRAME") {
                continue;
            }
            let height = RE_RESOLUTION
                .captures(line)
                .and_then(|c| c[1].parse().ok())
                .unwrap_or(0);
            let Some(variant) = lines.get(i + 1).map(|l| l.trim()) else {
                continue;
            };
            if variant.is_empty() || variant.starts_with('#') {
                continue;
            }
            let url = if variant.starts_with("http") {
                variant.to_string()
            } else {
                let base = master_url.rsplit_once('/').map(|(b, _)| b).unwrap_or("");
                format!("{base}/{variant}")
            };
            streams.push(Stream {
                height,
                url,
                referer: ANIDB_REFERER.to_string(),
                provider: "anidb".to_string(),
            });
        }
        if streams.is_empty() {
            // Master may already be a media playlist — treat as single unknown quality.
            streams.push(Stream {
                height: 0,
                url: master_url.to_string(),
                referer: ANIDB_REFERER.to_string(),
                provider: "anidb".to_string(),
            });
        }
        Ok(streams)
    }
}

fn show_numeric_id(show_id: &str) -> Option<&str> {
    let tail = show_id.rsplit('-').next()?;
    if tail.chars().all(|c| c.is_ascii_digit()) {
        Some(tail)
    } else {
        None
    }
}

fn ep_sort_key(ep: &str) -> f64 {
    ep.parse().unwrap_or(f64::MAX)
}

fn html_unescape(s: &str) -> String {
    s.replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

#[derive(Debug, Deserialize)]
struct EpisodesResponse {
    episodes: Vec<EpisodeEntry>,
}

#[derive(Debug, Deserialize)]
struct EpisodeEntry {
    id: u64,
    number: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct LanguagesResponse {
    languages: Vec<LanguageEntry>,
}

#[derive(Debug, Deserialize)]
struct LanguageEntry {
    code: String,
    embed_url: String,
}
