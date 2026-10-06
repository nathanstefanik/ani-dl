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
use tokio::io::{AsyncWrite, AsyncWriteExt};

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
        eprintln!("  HLS download (referer: {})", self.referer);
        let p = self.prepare(url, quality).await?;
        if p.media_url != url {
            eprintln!("  variant playlist: {}", p.media_url);
        }
        let n = p.jobs.len();
        eprintln!(
            "  {} segments (~{:.0} min video), {} AES key(s), {} parallel",
            n,
            p.secs / 60.0,
            p.key_cache.len(),
            self.concurrency
        );
        let pb = progress_bar(n as u64, "segments");
        pb.set_style(
            ProgressStyle::with_template("  {msg} [{bar:30}] {pos}/{len} ({eta})")
                .unwrap()
                .progress_chars("=> "),
        );

        // Segments are checked as MPEG-TS (decrypted first when the playlist
        // says so), then written in playlist order as they finish into a .part
        // file that is renamed to the .mp4 only on success — so memory stays
        // around `concurrency` segments and an interrupted download never
        // looks like a finished .mp4.
        let part_path = out_path.with_extension("mp4.part");
        let mut file = tokio::fs::File::create(&part_path).await?;
        let key_cache = &p.key_cache;
        let segments = p.jobs.into_iter().map(|job| {
            let pb = pb.clone();
            async move {
                let data =
                    download_segment(&self.client, &job, &self.referer, self.retries, key_cache)
                        .await
                        .with_context(|| format!("segment {} ({})", job.index, job.url));
                pb.inc(1);
                data
            }
        });
        let result = write_segments(segments, self.concurrency, &mut file).await;
        pb.finish_and_clear();
        match result {
            Ok(()) => {
                tokio::fs::rename(&part_path, out_path).await?;
                Ok(out_path.to_path_buf())
            }
            Err(e) => {
                drop(file);
                let _ = tokio::fs::remove_file(&part_path).await;
                Err(anyhow!("segment download failed: {e:#}"))
            }
        }
    }

    /// Download and check only the first segment of an HLS playlist, returning its size.
    /// `ani-dl sync` uses this to show a download would actually work.
    pub async fn probe(&self, url: &str) -> Result<usize> {
        let p = self.prepare(url, "best").await?;
        let data = download_segment(
            &self.client,
            &p.jobs[0],
            &self.referer,
            self.retries,
            &p.key_cache,
        )
        .await?;
        Ok(data.len())
    }

    /// Steps 1–3 of an HLS download: resolve the variant, fetch and parse the
    /// media playlist, pre-fetch keys and build the segment jobs. Prints nothing.
    async fn prepare(&self, url: &str, quality: &str) -> Result<Prepared> {
        // 1. Fetch playlist; if it's a master, pick a variant.
        let media_url = self.resolve_media_playlist(url, quality).await?;

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
        if media.segments.is_empty() {
            return Err(anyhow!("media playlist has no segments"));
        }

        let seg_base = base_url(&media_url);
        let start_seq = media.media_sequence;

        // 3. Pre-fetch any AES-128 keys referenced by the playlist.
        let key_cache = self.prefetch_keys(&media, &seg_base).await?;

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
        let secs: f32 = media.segments.iter().map(|s| s.duration).sum();
        Ok(Prepared {
            media_url,
            jobs,
            key_cache,
            secs,
        })
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

struct Prepared {
    media_url: String,
    jobs: Vec<SegmentJob>,
    key_cache: HashMap<String, Vec<u8>>,
    secs: f32,
}

struct SegmentJob {
    index: usize,
    url: String,
    key: Option<Key>,
    seq: u64,
}

const TS_PACKET: usize = 188;
const TS_SYNC: u8 = 0x47;

/// HLS segments here are MPEG-TS: back-to-back 188-byte packets that each start with 0x47.
fn check_ts(data: &[u8]) -> Result<()> {
    if data.len() >= TS_PACKET && data.chunks_exact(TS_PACKET).all(|p| p[0] == TS_SYNC) {
        return Ok(());
    }
    Err(anyhow!(
        "not an MPEG-TS segment ({} bytes, starts with {:02x?})",
        data.len(),
        &data[..data.len().min(8)]
    ))
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

/// Decrypt (when the playlist says so) then check the segment body.
fn finish_segment(
    bytes: Bytes,
    job: &SegmentJob,
    key_cache: &HashMap<String, Vec<u8>>,
) -> Result<Vec<u8>> {
    let data = maybe_decrypt(bytes, job, key_cache)?;
    check_ts(&data)?;
    Ok(data)
}

async fn download_segment(
    client: &wreq::Client,
    job: &SegmentJob,
    referer: &str,
    retries: u32,
    key_cache: &HashMap<String, Vec<u8>>,
) -> Result<Vec<u8>> {
    let mut last_err = None;
    for attempt in 0..=retries {
        // An invalid body (e.g. an HTML error page with HTTP 200) is retried
        // like a network error.
        let result: Result<Vec<u8>> = async {
            let bytes = client
                .get(&job.url)
                .header("Referer", referer)
                .send()
                .await?
                .error_for_status()?
                .bytes()
                .await?;
            finish_segment(bytes, job, key_cache)
        }
        .await;
        match result {
            Ok(data) => return Ok(data),
            Err(e) => last_err = Some(e),
        }
        if attempt < retries {
            eprintln!(
                "  ! retry {}/{} for {}: {:#}",
                attempt + 1,
                retries,
                job.url,
                last_err.as_ref().unwrap()
            );
            tokio::time::sleep(std::time::Duration::from_millis(300 * (attempt as u64 + 1))).await;
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("segment download failed")))
}

/// Write segments to `out` in playlist order as they finish, at most `concurrency` in flight.
/// Returns at the first error; dropping the stream cancels downloads still running.
async fn write_segments<F, W>(
    segments: impl IntoIterator<Item = F>,
    concurrency: usize,
    out: &mut W,
) -> Result<()>
where
    F: Future<Output = Result<Vec<u8>>>,
    W: AsyncWrite + Unpin,
{
    let mut ordered = stream::iter(segments).buffered(concurrency);
    while let Some(seg) = ordered.next().await {
        out.write_all(&seg?).await?;
    }
    out.flush().await?;
    Ok(())
}

fn pick_variant(variants: &[(u64, String)], quality: &str) -> String {
    // `variants` is sorted by height descending.
    match quality {
        "best" => variants[0].1.clone(),
        "worst" => variants.last().unwrap().1.clone(),
        q => {
            if let Some(want) = crate::providers::parse_quality_height(q) {
                let want = u64::from(want);
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

#[cfg(test)]
mod tests {
    use super::*;
    use cbc::cipher::BlockEncryptMut;
    use futures::future::{self, FutureExt};
    use std::time::Duration;

    fn ts_packet(fill: u8) -> [u8; TS_PACKET] {
        let mut p = [fill; TS_PACKET];
        p[0] = TS_SYNC;
        p
    }

    fn ts_segment(n: usize) -> Vec<u8> {
        (0..n).flat_map(|i| ts_packet(i as u8)).collect()
    }

    fn job(key: Option<Key>) -> SegmentJob {
        SegmentJob {
            index: 0,
            url: "http://example.test/seg_000.ts".to_string(),
            key,
            seq: 0,
        }
    }

    #[test]
    fn check_ts_accepts_back_to_back_packets() {
        assert!(check_ts(&ts_segment(3)).is_ok());
    }

    #[test]
    fn check_ts_tolerates_trailing_partial_packet() {
        let mut data = ts_segment(2);
        data.extend_from_slice(&[0xaa; 10]);
        assert!(check_ts(&data).is_ok());
    }

    #[test]
    fn check_ts_rejects_html_error_page() {
        let mut body = b"<html><body><h1>404 Not Found</h1></body></html>".to_vec();
        body.resize(500, b' ');
        let err = check_ts(&body).unwrap_err();
        assert!(err.to_string().contains("not an MPEG-TS segment"));
    }

    #[test]
    fn check_ts_rejects_png_signature_prefix() {
        let mut data = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        data.extend_from_slice(&ts_segment(2));
        assert!(check_ts(&data).is_err());
    }

    #[test]
    fn check_ts_rejects_bad_sync_inside_segment() {
        let mut data = ts_segment(3);
        data[TS_PACKET] = 0x00;
        assert!(check_ts(&data).is_err());
    }

    #[test]
    fn check_ts_rejects_short_or_empty_bodies() {
        assert!(check_ts(&[]).is_err());
        assert!(check_ts(&[TS_SYNC; 100]).is_err());
    }

    #[test]
    fn finish_segment_checks_after_decrypting() {
        let plaintext = ts_segment(2);
        let key = [7u8; 16];
        let iv = hex::decode("000102030405060708090a0b0c0d0e0f").unwrap();
        let ciphertext = cbc::Encryptor::<aes::Aes128>::new((&key).into(), iv.as_slice().into())
            .encrypt_padded_vec_mut::<Pkcs7>(&plaintext);
        // The ciphertext itself must not pass the check (AES is deterministic,
        // so this cannot flake).
        assert!(check_ts(&ciphertext).is_err());

        let key_cache = HashMap::from([("k".to_string(), key.to_vec())]);
        let job = job(Some(Key {
            method: KeyMethod::AES128,
            uri: Some("k".to_string()),
            iv: Some("0x000102030405060708090a0b0c0d0e0f".to_string()),
            ..Default::default()
        }));
        let out = finish_segment(Bytes::from(ciphertext), &job, &key_cache).unwrap();
        assert_eq!(out, plaintext);
    }

    #[test]
    fn finish_segment_rejects_unencrypted_html() {
        let job = job(None);
        let body = Bytes::from_static(b"<html><body>404</body></html>");
        assert!(finish_segment(body, &job, &HashMap::new()).is_err());
    }

    #[tokio::test]
    async fn write_segments_keeps_playlist_order() {
        let segments = vec![
            async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                Ok(vec![0u8; 3])
            }
            .boxed(),
            future::ready(Ok(vec![1u8; 3])).boxed(),
        ];
        let mut out = Vec::new();
        write_segments(segments, 2, &mut out).await.unwrap();
        assert_eq!(out, vec![0, 0, 0, 1, 1, 1]);
    }

    #[tokio::test]
    async fn write_segments_stops_at_first_error() {
        let segments = vec![
            future::ready(Ok::<Vec<u8>, anyhow::Error>(vec![1u8])).boxed(),
            future::ready(Err(anyhow!("boom"))).boxed(),
            future::pending::<Result<Vec<u8>>>().boxed(),
        ];
        let mut out = Vec::new();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            write_segments(segments, 3, &mut out),
        )
        .await
        .expect("write_segments hung on a pending segment");
        let err = result.unwrap_err();
        assert!(err.to_string().contains("boom"));
        assert_eq!(out, vec![1]);
    }
}
