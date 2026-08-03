//! m3u8 parsing + parallel HLS segment downloader (pure Rust, no ffmpeg).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, KeyIvInit};
use futures::stream::{self, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use m3u8_rs::{Key, KeyMethod, Playlist};
use tokio::io::AsyncWriteExt;

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

pub struct HlsDownloader {
    pub client: wreq::Client,
    pub concurrency: usize,
    pub referer: String,
    pub retries: u32,
}

impl HlsDownloader {
    pub fn new(client: wreq::Client, concurrency: usize, referer: String, retries: u32) -> Self {
        Self {
            client,
            concurrency,
            referer,
            retries,
        }
    }

    /// Download `url` to `out_path` (a `.mp4`). Handles HLS playlists and
    /// direct progressive files.
    pub async fn download(&self, url: &str, out_path: &Path, quality: &str) -> Result<PathBuf> {
        if url.contains(".m3u8") {
            self.download_hls(url, out_path, quality).await
        } else {
            self.download_direct(url, out_path).await
        }
    }

    async fn download_direct(&self, url: &str, out_path: &Path) -> Result<PathBuf> {
        eprintln!("  direct download (referer: {})", self.referer);
        let resp = self
            .client
            .get(url)
            .header("Referer", &self.referer)
            .send()
            .await
            .context("direct download request failed")?
            .error_for_status()?;
        let total = resp.content_length().unwrap_or(0);
        eprintln!(
            "  HTTP {} — size: {}",
            resp.status(),
            if total > 0 {
                indicatif::HumanBytes(total).to_string()
            } else {
                "unknown".to_string()
            }
        );
        let pb = progress_bar(total, "downloading");
        if total > 0 {
            pb.set_style(
                ProgressStyle::with_template(
                    "  {msg} [{bar:30}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
                )
                .unwrap()
                .progress_chars("=> "),
            );
        } else {
            pb.set_style(
                ProgressStyle::with_template("  {msg} {spinner} {bytes} ({bytes_per_sec})")
                    .unwrap(),
            );
        }

        // Stream into a .part file and rename on success, so an interrupted
        // download never leaves behind what looks like a finished .mp4.
        let part_path = out_path.with_extension("mp4.part");
        let mut file = tokio::fs::File::create(&part_path).await?;
        let mut stream = resp.bytes_stream();
        let mut downloaded = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    pb.finish_and_clear();
                    let _ = tokio::fs::remove_file(&part_path).await;
                    return Err(anyhow::Error::new(e).context(format!(
                        "body read failed after {} of {}",
                        indicatif::HumanBytes(downloaded),
                        if total > 0 {
                            indicatif::HumanBytes(total).to_string()
                        } else {
                            "unknown".to_string()
                        }
                    )));
                }
            };
            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;
            pb.set_position(downloaded);
        }
        file.flush().await?;
        pb.finish_and_clear();
        if total > 0 && downloaded < total {
            let _ = tokio::fs::remove_file(&part_path).await;
            return Err(anyhow!(
                "truncated download: got {} of {}",
                indicatif::HumanBytes(downloaded),
                indicatif::HumanBytes(total)
            ));
        }
        tokio::fs::rename(&part_path, out_path).await?;
        Ok(out_path.to_path_buf())
    }

    async fn download_hls(&self, url: &str, out_path: &Path, quality: &str) -> Result<PathBuf> {
        // 1. Fetch playlist; if it's a master, pick a variant.
        eprintln!("  HLS download (referer: {})", self.referer);
        let media_url = self.resolve_media_playlist(url, quality).await?;
        if media_url != url {
            eprintln!("  variant playlist: {media_url}");
        }

        // 2. Fetch the media playlist and collect segments.
        let text = self
            .client
            .get(&media_url)
            .header("Referer", &self.referer)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let playlist = m3u8_rs::parse_playlist_res(&text)
            .map_err(|e| anyhow!("parse media playlist: {e:?}"))?;
        let media = match playlist {
            Playlist::MediaPlaylist(m) => m,
            Playlist::MasterPlaylist(_) => {
                return Err(anyhow!("expected media playlist, got another master"))
            }
        };

        // Unsupported playlist styles would silently produce corrupt output:
        // without the EXT-X-MAP init segment the concatenation is unplayable,
        // and without Range headers each EXT-X-BYTERANGE fetch grabs the whole
        // file. Fail loudly instead.
        if media.segments.iter().any(|s| s.map.is_some() || s.byte_range.is_some()) {
            return Err(anyhow!("fMP4 / byte-range HLS playlists are not supported"));
        }

        let seg_base = base_url(&media_url);
        let start_seq = media.media_sequence;

        // 3. Pre-fetch any AES-128 keys referenced by the playlist.
        let key_cache = self.prefetch_keys(&media, &seg_base).await?;

        // 4. Download all segments in parallel, then concat in memory (no temp
        // segment files — writing each .ts to disk and reading it back doubled I/O).
        let n = media.segments.len();
        let dur: f32 = media.segments.iter().map(|s| s.duration).sum();
        eprintln!(
            "  {} segments (~{:.0} min video), {} AES key(s), {} parallel",
            n,
            dur / 60.0,
            key_cache.len(),
            self.concurrency
        );
        let pb = progress_bar(n as u64, "segments");
        pb.set_style(
            ProgressStyle::with_template("  {msg} [{bar:30}] {pos}/{len} ({eta})")
                .unwrap()
                .progress_chars("=> "),
        );

        // EXT-X-KEY is sticky: it applies to every subsequent segment until the
        // next EXT-X-KEY. m3u8-rs does not always propagate it, so carry it
        // forward ourselves.
        let mut current_key: Option<Key> = None;
        let mut jobs: Vec<SegmentJob> = Vec::with_capacity(media.segments.len());
        for (i, seg) in media.segments.iter().enumerate() {
            if seg.key.is_some() {
                current_key = seg.key.clone();
            }
            jobs.push(SegmentJob {
                index: i,
                url: join_url(&seg_base, &seg.uri),
                key: current_key.clone(),
                seq: start_seq + i as u64,
            });
        }

        let results = stream::iter(jobs.into_iter().map(|job| {
            let client = self.client.clone();
            let referer = self.referer.clone();
            let retries = self.retries;
            let key_cache = &key_cache;
            let pb = pb.clone();
            async move {
                let bytes = download_segment(&client, &job.url, &referer, retries)
                    .await
                    .with_context(|| format!("segment {} ({})", job.index, job.url))?;
                let bytes = maybe_decrypt(bytes, &job, key_cache)?;
                pb.inc(1);
                Ok::<(usize, Vec<u8>), anyhow::Error>((job.index, bytes))
            }
        }))
        .buffer_unordered(self.concurrency)
        .collect::<Vec<_>>()
        .await;

        let mut segments: Vec<(usize, Vec<u8>)> = Vec::with_capacity(n);
        for r in results {
            match r {
                Ok(seg) => segments.push(seg),
                Err(e) => {
                    pb.finish_and_clear();
                    return Err(anyhow!("segment download failed: {e:#}"));
                }
            }
        }
        pb.finish_and_clear();
        segments.sort_by_key(|(i, _)| *i);

        // 5. Concatenate in order into a .part file, then rename, so a crash
        // mid-concat never leaves a half-written .mp4.
        eprintln!("  concatenating {n} segments...");
        let part_path = out_path.with_extension("mp4.part");
        let mut out = tokio::fs::File::create(&part_path).await?;
        for (_, data) in segments {
            out.write_all(&data).await?;
        }
        out.flush().await?;
        tokio::fs::rename(&part_path, out_path).await?;
        Ok(out_path.to_path_buf())
    }

    async fn resolve_media_playlist(&self, url: &str, quality: &str) -> Result<String> {
        let text = self
            .client
            .get(url)
            .header("Referer", &self.referer)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        match m3u8_rs::parse_playlist_res(&text) {
            Ok(Playlist::MasterPlaylist(master)) => {
                let base = base_url(url);
                let mut variants: Vec<(u64, String)> = master
                    .variants
                    .iter()
                    .map(|v| {
                        let h = v.resolution.map(|r| r.height).unwrap_or(0);
                        (h, join_url(&base, &v.uri))
                    })
                    .collect();
                if variants.is_empty() {
                    return Err(anyhow!("master playlist has no variants"));
                }
                variants.sort_by_key(|v| std::cmp::Reverse(v.0));
                let chosen = pick_variant(&variants, quality);
                Ok(chosen)
            }
            // Already a media playlist — download it directly.
            Ok(Playlist::MediaPlaylist(_)) => Ok(url.to_string()),
            Err(e) => Err(anyhow!("parse master playlist: {e:?}")),
        }
    }

    async fn prefetch_keys(
        &self,
        media: &m3u8_rs::MediaPlaylist,
        seg_base: &str,
    ) -> Result<HashMap<String, Vec<u8>>> {
        let mut cache: HashMap<String, Vec<u8>> = HashMap::new();
        let mut current_key: Option<Key> = None;
        for seg in &media.segments {
            if seg.key.is_some() {
                current_key = seg.key.clone();
            }
            let Some(key) = &current_key else { continue };
            if key.method != KeyMethod::AES128 {
                continue;
            }
            let Some(uri) = &key.uri else { continue };
            if cache.contains_key(uri) {
                continue;
            }
            let key_url = join_url(seg_base, uri);
            let bytes = self
                .client
                .get(&key_url)
                .header("Referer", &self.referer)
                .send()
                .await?
                .error_for_status()?
                .bytes()
                .await?;
            cache.insert(uri.clone(), bytes.to_vec());
        }
        Ok(cache)
    }
}

struct SegmentJob {
    index: usize,
    url: String,
    key: Option<Key>,
    seq: u64,
}

fn maybe_decrypt(
    bytes: Bytes,
    job: &SegmentJob,
    key_cache: &HashMap<String, Vec<u8>>,
) -> Result<Vec<u8>> {
    let Some(key) = &job.key else {
        return Ok(bytes.to_vec());
    };
    if key.method != KeyMethod::AES128 {
        return Ok(bytes.to_vec());
    }
    let Some(uri) = &key.uri else {
        return Ok(bytes.to_vec());
    };
    let key_bytes = key_cache
        .get(uri)
        .ok_or_else(|| anyhow!("AES key not prefetched for {uri}"))?;
    if key_bytes.len() != 16 {
        return Err(anyhow!("AES-128 key must be 16 bytes, got {}", key_bytes.len()));
    }

    // IV: explicit from the playlist, else the segment sequence number (BE).
    let iv = match &key.iv {
        Some(iv_hex) => {
            let h = iv_hex.trim_start_matches("0x").trim_start_matches("0X");
            hex::decode(h).map_err(|e| anyhow!("bad IV hex: {e}"))?
        }
        None => {
            let mut iv = [0u8; 16];
            iv[8..].copy_from_slice(&job.seq.to_be_bytes());
            iv.to_vec()
        }
    };
    if iv.len() != 16 {
        return Err(anyhow!("IV must be 16 bytes"));
    }

    let plain = Aes128CbcDec::new(key_bytes.as_slice().into(), iv.as_slice().into())
        .decrypt_padded_vec_mut::<Pkcs7>(&bytes)
        .map_err(|e| anyhow!("AES-128-CBC decrypt: {e}"))?;
    Ok(plain)
}

async fn download_segment(
    client: &wreq::Client,
    url: &str,
    referer: &str,
    retries: u32,
) -> Result<Bytes> {
    let mut last_err = None;
    for attempt in 0..=retries {
        match client.get(url).header("Referer", referer).send().await {
            Ok(resp) => match resp.error_for_status() {
                Ok(resp) => match resp.bytes().await {
                    Ok(b) => return Ok(b),
                    Err(e) => last_err = Some(anyhow!(e)),
                },
                Err(e) => last_err = Some(anyhow!(e)),
            },
            Err(e) => last_err = Some(anyhow!(e)),
        }
        if attempt < retries {
            eprintln!(
                "  ! retry {}/{} for {url}: {:#}",
                attempt + 1,
                retries,
                last_err.as_ref().unwrap()
            );
            tokio::time::sleep(std::time::Duration::from_millis(300 * (attempt as u64 + 1))).await;
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("segment download failed")))
}

fn pick_variant(variants: &[(u64, String)], quality: &str) -> String {
    // `variants` is sorted by height descending.
    match quality {
        "best" => variants[0].1.clone(),
        "worst" => variants.last().unwrap().1.clone(),
        q => {
            if let Ok(want) = q.parse::<u64>() {
                if let Some(v) = variants.iter().find(|(h, _)| *h == want) {
                    return v.1.clone();
                }
                if let Some(v) = variants.iter().find(|(h, _)| *h > 0 && *h <= want) {
                    return v.1.clone();
                }
            }
            variants[0].1.clone()
        }
    }
}

fn base_url(url: &str) -> String {
    // Strip query, then the last path segment.
    let no_query = url.split('?').next().unwrap_or(url);
    no_query.rsplit_once('/').map(|(b, _)| b).unwrap_or("").to_string()
}

fn join_url(base: &str, uri: &str) -> String {
    if uri.starts_with("http://") || uri.starts_with("https://") {
        uri.to_string()
    } else if let Some(rest) = uri.strip_prefix('/') {
        // Absolute path: keep scheme+host from base.
        if let Some(idx) = base.find("://") {
            let after = &base[idx + 3..];
            let host = after.split('/').next().unwrap_or(after);
            format!("{}://{}/{}", &base[..idx], host, rest)
        } else {
            format!("{base}/{rest}")
        }
    } else {
        format!("{base}/{uri}")
    }
}

fn progress_bar(total: u64, msg: &'static str) -> ProgressBar {
    let pb = if total > 0 {
        ProgressBar::new(total)
    } else {
        ProgressBar::new_spinner()
    };
    pb.set_message(msg);
    pb
}
