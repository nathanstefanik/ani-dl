//! anidb.app client (ani-cli v5 provider). Uses wreq Chrome TLS emulation to
//! clear Cloudflare; plain reqwest gets 403.

use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use regex::Regex;
use serde::Deserialize;
use wreq::header::{HeaderMap, HeaderValue, USER_AGENT as H_UA};
use wreq_util::Emulation;

use crate::constants::{ANIDB_BASE, ANIDB_REFERER, USER_AGENT};
use crate::providers::Stream;

static RE_SHOW_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"anime/([^"/]+-[0-9]+)""#).expect("show id regex"));
static RE_TITLE_ATTR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"title="([^"]+)""#).expect("title attr regex"));
static RE_ALT_ATTR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"alt="([^"]+)""#).expect("alt attr regex"));
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

    /// GET with ani-cli 5.0.4 `anidb_curl` semantics: a transport failure, a
    /// non-2xx status, a Cloudflare interstitial and an empty body are four
    /// distinct errors, each named in the message.
    async fn get_text(&self, url: &str) -> Result<String> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| anyhow!("Connection error: could not fetch {url} ({e})"))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| anyhow!("Connection error: could not read {url} ({e})"))?;

        // Cloudflare answers the challenge with its own status code; name the
        // challenge rather than the code, as ani-cli does.
        if is_cloudflare_challenge(&body) {
            anyhow::bail!(
                "Blocked by Cloudflare on {url}; TLS fingerprint may need updating (wreq Emulation)"
            );
        }
        if !status.is_success() {
            let hint = if status.as_u16() == 503 {
                " (anidb.app may be under maintenance)"
            } else {
                ""
            };
            anyhow::bail!("Request failed: HTTP {} from {url}{hint}", status.as_u16());
        }
        if body.trim().is_empty() {
            anyhow::bail!("Connection error: no response from {ANIDB_BASE} ({url})");
        }
        Ok(body)
    }

    pub async fn search(&self, query: &str, _mode: TranslationType) -> Result<Vec<ShowResult>> {
        let q = query.trim().replace(' ', "+");
        let url = format!("{ANIDB_BASE}/browse?q={q}");
        let html = self.get_text(&url).await?;

        Ok(parse_search_results(&html))
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

/// Pull `(slug, title)` pairs out of a browse page. Split on `<a href` first,
/// as ani-cli does, so the title is read from the same card as the link — the
/// card exposes it as `title=` on the anchor or `alt=` on the poster `img`.
fn parse_search_results(html: &str) -> Vec<ShowResult> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for card in html.split("<a href") {
        let Some(id_match) = RE_SHOW_ID.captures(card) else {
            continue;
        };
        let id = id_match[1].to_string();
        // Only look past the link itself, so a neighbouring card's attributes
        // cannot be picked up as this show's title.
        let rest = &card[id_match.get(0).map_or(0, |m| m.end())..];
        let Some(name) = RE_TITLE_ATTR
            .captures(rest)
            .or_else(|| RE_ALT_ATTR.captures(rest))
            .map(|c| html_unescape(&c[1]))
        else {
            continue;
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        out.push(ShowResult {
            id,
            name,
            episodes: 0,
            year: 0,
        });
    }
    out
}

fn is_cloudflare_challenge(body: &str) -> bool {
    body.contains("Just a moment") || body.contains("cf-mitigated")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_reads_title_attribute() {
        let html = r#"<div><a href="/anime/sousou-no-frieren-1234" title="Sousou no Frieren">
            <img src="/p.jpg" alt="poster"></a></div>"#;
        let out = parse_search_results(html);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "sousou-no-frieren-1234");
        assert_eq!(out[0].name, "Sousou no Frieren");
    }

    #[test]
    fn search_falls_back_to_img_alt() {
        // ani-cli 5.x reads alt=; keep working if the anchor drops title=.
        let html = r#"<a href="/anime/fruits-basket-99" class="card">
            <img src="/p.jpg" alt="Fruits Basket &amp; Friends"></a>"#;
        let out = parse_search_results(html);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "Fruits Basket & Friends");
    }

    #[test]
    fn search_dedupes_and_ignores_non_show_links() {
        let html = r#"<a href="/browse?q=x" title="Browse"></a>
            <a href="/anime/bocchi-the-rock-7" title="Bocchi the Rock!"></a>
            <a href="/anime/bocchi-the-rock-7" title="Bocchi the Rock!"></a>"#;
        let out = parse_search_results(html);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "bocchi-the-rock-7");
    }

    #[test]
    fn title_is_not_borrowed_from_the_previous_card() {
        // The title attribute sits before the href here, so this card has no
        // name of its own and must be skipped rather than stealing one.
        let html = r#"<a title="Wrong Show" href="/anime/right-show-1"></a>"#;
        assert!(parse_search_results(html).is_empty());
    }

    #[test]
    fn cloudflare_challenge_is_recognised() {
        assert!(is_cloudflare_challenge("<title>Just a moment...</title>"));
        assert!(is_cloudflare_challenge("cf-mitigated: challenge"));
        assert!(!is_cloudflare_challenge("<html>Under Maintenance</html>"));
    }
}
