//! hianime.at client (ani-cli 5.1.2 provider). Uses wreq Chrome TLS emulation to
//! clear Cloudflare; plain reqwest gets 403 on some hops.

use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use regex::Regex;
use serde::Deserialize;
use wreq::header::{HeaderMap, HeaderValue, USER_AGENT as H_UA};
use wreq_util::Emulation;

use crate::constants::{EMBED_XOR_KEY, HIANIME_BASE, HIANIME_REFERER, USER_AGENT};
use crate::providers::Stream;

static RE_FILM_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<h3 class="film-name">\s*<a href="[^"]*/([^"/]+)"\s+title="([^"]+)""#)
        .expect("film name regex")
});
static RE_EPS_IN_CARD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\((\d+) eps\)").expect("eps in card regex"));
static RE_EP_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"data-number="([^"]*)""#).expect("ep number regex"));
static RE_EP_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"data-id="([0-9]+)""#).expect("ep id regex"));
static RE_DATA_TYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"data-type="([^"]+)""#).expect("data-type regex"));
static RE_SERVER_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"data-server-name="([^"]+)""#).expect("server name regex"));
static RE_SERVER_HASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"data-hash="([^"]+)""#).expect("server hash regex"));
static RE_EMBED_BLOB: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"window\.__P="([^"]*)""#).expect("embed blob regex"));
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
}

#[derive(Debug, Clone)]
pub struct ShowResult {
    /// Slug id, e.g. `cyberpunk-edgerunners-1048`.
    pub id: String,
    pub name: String,
    /// From the search card (`(N eps)`); 0 if the card does not show a count.
    pub episodes: u32,
    /// Not available from search cards; always 0 for now.
    pub year: u32,
}

pub struct HianimeClient {
    pub client: wreq::Client,
}

fn default_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(H_UA, HeaderValue::from_static(USER_AGENT));
    headers
}

/// Shared wreq client with Chrome TLS fingerprint. Used for both API and
/// downloads — hianime.at (and the ZokoAnime HLS CDN) sit behind Cloudflare.
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

impl HianimeClient {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: http_client()?,
        })
    }

    /// GET with ani-cli 5.1.2 `hianime_curl` semantics: a transport failure, a
    /// non-2xx status, a Cloudflare interstitial and an empty body are distinct
    /// errors, each named in the message.
    async fn get_text(&self, url: &str) -> Result<String> {
        self.get_text_referer(url, None).await
    }

    async fn get_text_referer(&self, url: &str, referer: Option<&str>) -> Result<String> {
        let mut req = self.client.get(url);
        if let Some(r) = referer {
            req = req.header("Referer", r);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| anyhow!("Connection error: could not fetch {url} ({e})"))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| anyhow!("Connection error: could not read {url} ({e})"))?;

        if is_cloudflare_challenge(&body) {
            anyhow::bail!(
                "Blocked by Cloudflare on {url}; TLS fingerprint may need updating (wreq Emulation)"
            );
        }
        if !status.is_success() {
            anyhow::bail!("Request failed: HTTP {} from {url}", status.as_u16());
        }
        if body.trim().is_empty() {
            anyhow::bail!("Connection error: no response from {HIANIME_BASE} ({url})");
        }
        Ok(body)
    }

    pub async fn search(&self, query: &str, _mode: TranslationType) -> Result<Vec<ShowResult>> {
        let q = query.trim().replace(' ', "+");
        let url = format!("{HIANIME_BASE}/search?keyword={q}");
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
        let url = format!("{HIANIME_BASE}/api/theme/episode/list/{numeric}");
        let text = self.get_text(&url).await?;
        Ok(parse_episode_maps(&envelope_html(&text), show_id))
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

        let url = format!("{HIANIME_BASE}/api/theme/episode/servers?episodeId={ep_id}");
        let text = self.get_text(&url).await?;
        let embed = parse_zoko_embed(&envelope_html(&text), mode.as_str())
            .ok_or_else(|| anyhow!("No sources found for {}!", mode.as_str()))?;

        let referer = origin_referer(&embed);
        let embed_html = self.get_text(&embed).await?;
        let blob = RE_EMBED_BLOB
            .captures(&embed_html)
            .map(|c| c[1].to_string())
            .ok_or_else(|| anyhow!("no window.__P blob in embed page"))?;
        let json = deobfuscate_blob(&blob)?;
        let (master, subtitle) = parse_embed_config(&json)?;
        self.expand_master(&master, &referer, subtitle).await
    }

    async fn expand_master(
        &self,
        master_url: &str,
        referer: &str,
        subtitle: Option<String>,
    ) -> Result<Vec<Stream>> {
        let text = self.get_text_referer(master_url, Some(referer)).await?;
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
                referer: referer.to_string(),
                provider: "hianime".to_string(),
                subtitle: subtitle.clone(),
            });
        }
        if streams.is_empty() {
            streams.push(Stream {
                height: 0,
                url: master_url.to_string(),
                referer: referer.to_string(),
                provider: "hianime".to_string(),
                subtitle,
            });
        }
        Ok(streams)
    }
}

/// Pull `(slug, title)` pairs out of a search page. Cut off the "Top 10"
/// sidebar first, as ani-cli does, then split on film-detail cards so a
/// neighbouring card cannot donate its title.
fn parse_search_results(html: &str) -> Vec<ShowResult> {
    let html = html.split(r#"id="main-sidebar""#).next().unwrap_or(html);
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for card in html.split(r#"<div class="film-detail">"#).skip(1) {
        let Some(caps) = RE_FILM_NAME.captures(card) else {
            continue;
        };
        let id = caps[1].to_string();
        let name = html_unescape(&caps[2]);
        if !seen.insert(id.clone()) {
            continue;
        }
        let episodes = RE_EPS_IN_CARD
            .captures(card)
            .and_then(|c| c[1].parse().ok())
            .unwrap_or(0);
        out.push(ShowResult {
            id,
            name,
            episodes,
            year: 0,
        });
    }
    out
}

fn parse_episode_maps(html: &str, show_id: &str) -> Vec<(u64, String)> {
    let needle = format!("/watch/{show_id}?ep=");
    let mut maps = Vec::new();
    for item in html.split("ep-item").skip(1) {
        if !item.contains(&needle) {
            continue;
        }
        let Some(num) = RE_EP_NUMBER.captures(item) else {
            continue;
        };
        let Some(id) = RE_EP_ID.captures(item) else {
            continue;
        };
        let Ok(ep_id) = id[1].parse() else {
            continue;
        };
        maps.push((ep_id, num[1].to_string()));
    }
    maps.sort_by(|a, b| {
        ep_sort_key(&a.1)
            .partial_cmp(&ep_sort_key(&b.1))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    maps
}

fn parse_zoko_embed(html: &str, mode: &str) -> Option<String> {
    // Split on server-item like ani-cli, then read attributes independently so
    // extra data-* fields between type/name/hash cannot hide ZokoAnime.
    for item in html.split("server-item").skip(1) {
        let dtype = RE_DATA_TYPE.captures(item).map(|c| c[1].to_string());
        let name = RE_SERVER_NAME.captures(item).map(|c| c[1].to_string());
        let hash = RE_SERVER_HASH.captures(item).map(|c| c[1].to_string());
        if dtype.as_deref() != Some(mode) || name.as_deref() != Some("ZokoAnime") {
            continue;
        }
        let Some(hash) = hash else { continue };
        return b64_decode(&hash)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .filter(|s| !s.is_empty());
    }
    None
}

/// Video `src` is required; a malformed subtitle object must not fail the episode.
fn parse_embed_config(json: &str) -> Result<(String, Option<String>)> {
    let v: serde_json::Value =
        serde_json::from_str(json).context("parsing deobfuscated embed JSON")?;
    let src = v
        .get("src")
        .and_then(|s| s.as_str())
        .filter(|s| s.contains(".m3u8"))
        .ok_or_else(|| anyhow!("embed src is not m3u8"))?
        .to_string();
    let subtitle = v.get("subtitles").and_then(|s| s.as_array()).and_then(|arr| {
        arr.iter()
            .find(|t| t.get("default").and_then(|d| d.as_bool()) == Some(true))
            .and_then(|t| t.get("src").and_then(|s| s.as_str()))
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    });
    Ok((src, subtitle))
}

fn envelope_html(text: &str) -> String {
    serde_json::from_str::<HtmlEnvelope>(text)
        .map(|e| e.html)
        .unwrap_or_else(|_| text.replace('\\', ""))
}

pub fn deobfuscate_blob(b64: &str) -> Result<String> {
    let raw = b64_decode(b64)?;
    let xor: Vec<u8> = raw
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ EMBED_XOR_KEY[i % EMBED_XOR_KEY.len()])
        .collect();
    String::from_utf8(xor).context("deobfuscated blob is not utf-8")
}

fn b64_decode(s: &str) -> Result<Vec<u8>> {
    let mut padded = s.trim().to_string();
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    B64.decode(padded.as_bytes())
        .map_err(|e| anyhow!("base64 decode: {e}"))
}

fn origin_referer(url: &str) -> String {
    if let Some(scheme_end) = url.find("://") {
        let after = &url[scheme_end + 3..];
        let host = after.split('/').next().unwrap_or(after);
        format!("{}{host}/", &url[..scheme_end + 3])
    } else {
        HIANIME_REFERER.to_string()
    }
}

fn is_cloudflare_challenge(body: &str) -> bool {
    body.contains("<title>Just a moment") || body.contains("cf-mitigated")
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
struct HtmlEnvelope {
    html: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_reads_film_name_and_cuts_sidebar() {
        let html = r#"
            <div class="film-detail">
                <h3 class="film-name">
                    <a href="https://hianime.at/sousou-no-frieren-1234"
                        title="Sousou no Frieren">Sousou no Frieren</a>
                </h3>
                <div class="fd-infor"><span class="fdi-item">TV (28 eps)</span></div>
            </div>
            <div id="main-sidebar">
                <div class="film-detail">
                    <h3 class="film-name">
                        <a href="https://hianime.at/junk-1" title="Sidebar Junk">nope</a>
                    </h3>
                </div>
            </div>"#;
        let out = parse_search_results(html);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "sousou-no-frieren-1234");
        assert_eq!(out[0].name, "Sousou no Frieren");
        assert_eq!(out[0].episodes, 28);
    }

    #[test]
    fn search_unescapes_title_entities() {
        let html = r#"<div class="film-detail">
            <h3 class="film-name">
                <a href="https://hianime.at/fruits-basket-99"
                    title="Fruits Basket &amp; Friends">x</a>
            </h3></div>"#;
        let out = parse_search_results(html);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "Fruits Basket & Friends");
    }

    #[test]
    fn search_dedupes() {
        let html = r#"
            <div class="film-detail">
                <h3 class="film-name">
                    <a href="https://hianime.at/bocchi-the-rock-7" title="Bocchi the Rock!">a</a>
                </h3>
            </div>
            <div class="film-detail">
                <h3 class="film-name">
                    <a href="https://hianime.at/bocchi-the-rock-7" title="Bocchi the Rock!">a</a>
                </h3>
            </div>"#;
        let out = parse_search_results(html);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "bocchi-the-rock-7");
    }

    #[test]
    fn episode_maps_require_matching_slug() {
        let html = r#"
            <a class="ep-item" data-number="1" data-id="17575"
               href="https://hianime.at/watch/cyberpunk-edgerunners-1048?ep=17575"></a>
            <a class="ep-item" data-number="6" data-id="99"
               href="https://hianime.at/watch/one-piece-100?ep=99"></a>"#;
        let maps = parse_episode_maps(html, "cyberpunk-edgerunners-1048");
        assert_eq!(maps, vec![(17575, "1".into())]);
        assert!(parse_episode_maps(html, "one-piece-1").is_empty());
    }

    #[test]
    fn zoko_embed_picks_requested_mode() {
        let html = r#"
            <div class="server-item" data-type="sub" data-id="4"
                data-server-name="ZokoAnime"
                data-hash="aHR0cHM6Ly96b2tvYW5pbWUudmlkZW8vc3Vi">x</div>
            <div class="server-item" data-type="dub"
                data-server-name="ZokoAnime"
                data-hash="aHR0cHM6Ly96b2tvYW5pbWUudmlkZW8vZHVi">x</div>
            <div class="server-item" data-type="sub"
                data-server-name="HD-1"
                data-hash="aHR0cHM6Ly9tZWdhcGxheS5idXp6L3N1Yg">x</div>"#;
        assert_eq!(
            parse_zoko_embed(html, "sub").as_deref(),
            Some("https://zokoanime.video/sub")
        );
        assert_eq!(
            parse_zoko_embed(html, "dub").as_deref(),
            Some("https://zokoanime.video/dub")
        );
    }

    #[test]
    fn deobfuscate_round_trips_otaku_embed_key() {
        let b64 = "FFYSGRYPX08KERBdBQtAWwQTFEAVAQdLB0IbHgIVEh8QX0sAURBcD1oTHAEDHxxZCQgRR152DRMcBgJJTw8NGRYVFxdZHgoMAAYFQQBDAQoJAhNfQQIVH1cBRwkHAwVYGkVNThUZAEgYQRlHF18VE1VWCR8BXRZXTUoBVRdcHxgERRZCCEIHFkpbAkVNWEMPEEsEGA4RRhcQUAMHBBYoUA==";
        let json = deobfuscate_blob(b64).unwrap();
        let (src, sub) = parse_embed_config(&json).unwrap();
        assert_eq!(src, "https://example.com/master.m3u8");
        assert_eq!(sub.as_deref(), Some("https://example.com/en.vtt"));
    }

    #[test]
    fn origin_referer_keeps_scheme_and_host() {
        assert_eq!(
            origin_referer("https://zokoanime.video/stream/mal/1/sub"),
            "https://zokoanime.video/"
        );
    }

    #[test]
    fn cloudflare_challenge_is_recognised() {
        assert!(is_cloudflare_challenge("<title>Just a moment...</title>"));
        assert!(is_cloudflare_challenge("cf-mitigated: challenge"));
        assert!(!is_cloudflare_challenge("<html>Under Maintenance</html>"));
    }
}
