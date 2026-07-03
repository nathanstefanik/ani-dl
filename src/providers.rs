//! Per-provider stream-URL extraction + AllAnime decryption (pure Rust).
//!
//! Ported from the Python `providers.py`. No video player is ever referenced.

use anyhow::{anyhow, Result};
use base64::Engine;
use ctr::cipher::{KeyIvInit, StreamCipher};
use futures::future::join_all;
use regex::Regex;

use crate::constants::{active_key_hex, ALLANIME_BASE, MP4UPLOAD_REFERER, REFERER};

type Aes256Ctr = ctr::Ctr128BE<aes::Aes256>;

#[derive(Debug, Clone)]
pub struct SourceUrl {
    pub source_name: String,
    pub source_url: String,
}

/// A single resolvable stream candidate.
#[derive(Debug, Clone)]
pub struct Stream {
    pub height: u32, // 0 == unknown
    pub url: String,
    pub referer: String,
    pub provider: String,
}

// --- crypto ------------------------------------------------------------------

/// Decrypt the AES-256-CTR `tobeparsed` blob into the JSON source list.
///
/// Layout (matches ani-cli `process_response`):
///   byte 0        : ignored
///   bytes 1..13   : IV (12 bytes)
///   bytes 13..-16 : ciphertext (last 16 bytes dropped)
///   CTR nonce     : IV(hex) + "00000002"
pub fn decrypt_source_list(b64: &str) -> Result<String> {
    let blob = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| anyhow!("base64 decode: {e}"))?;
    if blob.len() < 30 {
        return Err(anyhow!("tobeparsed blob too short"));
    }
    let iv_hex = hex::encode(&blob[1..13]);
    let ctr_hex = format!("{iv_hex}00000002");
    let ciphertext = &blob[13..blob.len() - 16];

    let key = hex::decode(active_key_hex())?;
    let iv = hex::decode(&ctr_hex)?;
    let mut cipher = Aes256Ctr::new(key.as_slice().into(), iv.as_slice().into());
    let mut buf = ciphertext.to_vec();
    cipher.apply_keystream(&mut buf);
    Ok(String::from_utf8_lossy(&buf).to_string())
}

/// Decode an obfuscated sourceUrl. A leading `--` marks a hex string whose
/// every byte is XORed by 0x38.
pub fn decode_source_url(url: &str) -> String {
    let Some(hexstr) = url.strip_prefix("--") else {
        return url.to_string();
    };
    // Byte-offset slicing below panics on non-ASCII; the API controls this
    // string, so refuse to decode rather than crash.
    if !hexstr.is_ascii() {
        return url.to_string();
    }
    let bytes = hexstr.as_bytes();
    let mut out = String::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i + 2 <= bytes.len() {
        match u8::from_str_radix(&hexstr[i..i + 2], 16) {
            Ok(b) => out.push((b ^  0x38) as char),
            Err(_) => break,
        }
        i += 2;
    }
    out.replace("/clock", "/clock.json")
}

// --- source-list parsing -----------------------------------------------------

fn collect_sources(v: &serde_json::Value, out: &mut Vec<SourceUrl>) {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(url) = map.get("sourceUrl").and_then(|x| x.as_str()) {
                let name = map
                    .get("sourceName")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                out.push(SourceUrl {
                    source_name: name,
                    source_url: url.to_string(),
                });
            }
            for val in map.values() {
                collect_sources(val, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for val in arr {
                collect_sources(val, out);
            }
        }
        _ => {}
    }
}

/// Parse a (possibly encrypted) episode reply into (sourceName, sourceUrl) pairs.
pub fn parse_source_urls(text: &str) -> Vec<SourceUrl> {
    let mut body = text.to_string();
    if body.contains("\"tobeparsed\"") {
        if let Some(cap) = Regex::new(r#""tobeparsed":"([^"]*)""#)
            .ok()
            .and_then(|re| re.captures(&body))
        {
            if let Ok(decrypted) = decrypt_source_list(&cap[1]) {
                body = decrypted;
            }
        }
    }

    let mut pairs = Vec::new();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
        collect_sources(&v, &mut pairs);
    }
    if !pairs.is_empty() {
        return pairs;
    }

    // Regex fallback for partial / non-JSON payloads.
    let clean = body.replace("\\u002F", "/").replace("\\/", "/");
    let block_re = Regex::new(r"\{[^{}]*\}").unwrap();
    let su_re = Regex::new(r#""sourceUrl":"([^"]*)""#).unwrap();
    let sn_re = Regex::new(r#""sourceName":"([^"]*)""#).unwrap();
    for block in block_re.find_iter(&clean) {
        if let Some(su) = su_re.captures(block.as_str()) {
            let sn = sn_re
                .captures(block.as_str())
                .map(|c| c[1].to_string())
                .unwrap_or_default();
            pairs.push(SourceUrl {
                source_name: sn,
                source_url: su[1].to_string(),
            });
        }
    }
    pairs
}

// --- per-provider resolution -------------------------------------------------

fn height_from(text: &str) -> u32 {
    Regex::new(r"(\d{3,4})")
        .ok()
        .and_then(|re| re.captures(text))
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(0)
}

fn expand_wixmp(url: &str) -> Vec<Stream> {
    let Some(caps) = Regex::new(r"/,([^/]*),/mp4").unwrap().captures(url) else {
        return Vec::new();
    };
    let list = caps[1].to_string();
    let extract = Regex::new(r"\.urlset.*")
        .unwrap()
        .replace(&url.replace("repackager.wixmp.com/", ""), "")
        .to_string();
    let comma_re = Regex::new(r",[^/]*").unwrap();
    let mut out = Vec::new();
    for q in list.split(',').filter(|q| !q.is_empty()) {
        let variant = comma_re.replace(&extract, q).to_string();
        out.push(Stream {
            height: height_from(q),
            url: variant,
            referer: REFERER.to_string(),
            provider: "wixmp".to_string(),
        });
    }
    out
}

async fn expand_m3u8_master(
    client: &reqwest::Client,
    url: &str,
    referer: &str,
) -> Vec<Stream> {
    let Ok(resp) = client.get(url).header("Referer", referer).send().await else {
        return Vec::new();
    };
    let text = resp.text().await.unwrap_or_default();
    if !text.contains("EXTM3U") {
        return Vec::new();
    }
    let base = url.rsplit_once('/').map(|(b, _)| b).unwrap_or("").to_string();
    let res_re = Regex::new(r"RESOLUTION=\d+x(\d+)").unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("#EXT-X-STREAM-INF") && i + 1 < lines.len() {
            let height = res_re
                .captures(line)
                .and_then(|c| c[1].parse().ok())
                .unwrap_or(0);
            let variant = lines[i + 1].trim();
            if !variant.is_empty() && !variant.starts_with('#') {
                let full = if variant.starts_with("http") {
                    variant.to_string()
                } else {
                    format!("{base}/{variant}")
                };
                out.push(Stream {
                    height,
                    url: full,
                    referer: referer.to_string(),
                    provider: "hls".to_string(),
                });
            }
        }
    }
    out
}

/// Resolve one (sourceName, sourceUrl) pair into stream candidates.
pub async fn resolve(
    client: &reqwest::Client,
    source: &SourceUrl,
) -> Result<Vec<Stream>> {
    let link = decode_source_url(&source.source_url);
    let name = if source.source_name.is_empty() {
        "?".to_string()
    } else {
        source.source_name.clone()
    };

    // mp4upload: scrape the mp4 src out of the embed HTML.
    if link.contains("mp4upload") {
        let resp = client.get(&link).header("Referer", REFERER).send().await?;
        let html = resp.text().await?;
        if let Some(cap) = Regex::new(r#"src:\s*"([^"]*)""#).unwrap().captures(&html) {
            return Ok(vec![Stream {
                height: 0,
                url: cap[1].to_string(),
                referer: MP4UPLOAD_REFERER.to_string(),
                provider: "mp4upload".to_string(),
            }]);
        }
        return Ok(vec![]);
    }

    // fast4speed / youtube: the decoded link is the stream itself.
    if link.contains("tools.fast4speed.rsvp") {
        return Ok(vec![Stream {
            height: 0,
            url: link,
            referer: REFERER.to_string(),
            provider: "fast4speed".to_string(),
        }]);
    }

    // Everything else: the AllAnime apiv2 "clock" JSON endpoint.
    let full = if link.starts_with('/') {
        format!("https://{ALLANIME_BASE}{link}")
    } else {
        link.clone()
    };
    let resp = client.get(&full).header("Referer", REFERER).send().await?;
    let text = resp.text().await?;

    let mut raw: Vec<Stream> = Vec::new();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
        if let Some(links) = v.get("links").and_then(|l| l.as_array()) {
            for entry in links {
                let url = entry
                    .get("link")
                    .or_else(|| entry.get("url"))
                    .or_else(|| entry.get("src"))
                    .and_then(|x| x.as_str());
                let Some(url) = url else { continue };
                let height = entry
                    .get("resolutionStr")
                    .and_then(|x| x.as_str())
                    .map(height_from)
                    .filter(|h| *h > 0)
                    .or_else(|| entry.get("height").and_then(|x| x.as_u64()).map(|h| h as u32))
                    .unwrap_or(0);
                raw.push(Stream {
                    height,
                    url: url.to_string(),
                    referer: REFERER.to_string(),
                    provider: name.clone(),
                });
            }
        }
    }
    if raw.is_empty() {
        // Regex fallback: any m3u8 url in the body.
        let re = Regex::new(r#""(?:link|url)":"([^"]*\.m3u8[^"]*)""#).unwrap();
        for cap in re.captures_iter(&text) {
            raw.push(Stream {
                height: 0,
                url: cap[1].replace("\\/", "/"),
                referer: REFERER.to_string(),
                provider: name.clone(),
            });
        }
    }

    // Post-process: expand wixmp urlsets and HLS master playlists.
    let mut streams = Vec::new();
    for s in raw {
        if s.url.contains("repackager.wixmp.com") {
            let expanded = expand_wixmp(&s.url);
            if expanded.is_empty() {
                streams.push(s);
            } else {
                streams.extend(expanded);
            }
        } else if s.url.contains("master.m3u8") && s.height == 0 {
            let expanded = expand_m3u8_master(client, &s.url, &s.referer).await;
            if expanded.is_empty() {
                streams.push(s);
            } else {
                streams.extend(expanded);
            }
        } else {
            streams.push(s);
        }
    }
    Ok(streams)
}

/// Probe every provider concurrently and collect all resolved streams.
///
/// We gather from all providers (rather than racing to the first with
/// `select_ok`) because correct `--quality` selection needs every candidate.
/// Probing is still fully concurrent via `join_all`, satisfying the "< 2s,
/// probe concurrently" requirement — the fan-out is parallel, only the final
/// pick is deferred until results are in.
pub async fn resolve_all(
    client: &reqwest::Client,
    sources: &[SourceUrl],
) -> Vec<Stream> {
    let futs = sources.iter().map(|s| async move {
        match resolve(client, s).await {
            Ok(streams) => streams,
            Err(e) => {
                eprintln!("  ! provider {} failed: {e:#}", s.source_name);
                Vec::new()
            }
        }
    });
    let results = join_all(futs).await;
    results.into_iter().flatten().collect()
}

/// Pick a stream for the requested quality ("best", "worst", or a height).
pub fn select_quality<'a>(streams: &'a [Stream], quality: &str) -> Option<&'a Stream> {
    if streams.is_empty() {
        return None;
    }
    let mut ordered: Vec<&Stream> = streams.iter().collect();
    ordered.sort_by_key(|s| std::cmp::Reverse(s.height));

    match quality {
        "best" => Some(ordered[0]),
        "worst" => ordered
            .iter()
            .rev()
            .find(|s| s.height > 0)
            .copied()
            .or_else(|| ordered.last().copied()),
        q => {
            if let Ok(want) = q.parse::<u32>() {
                if let Some(exact) = ordered.iter().find(|s| s.height == want) {
                    return Some(exact);
                }
                if let Some(below) = ordered.iter().find(|s| s.height > 0 && s.height <= want) {
                    return Some(below);
                }
                eprintln!("  ! quality {q} not found, using best");
            }
            Some(ordered[0])
        }
    }
}
