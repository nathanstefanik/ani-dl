//! AllAnime GraphQL client (async, connection-pooled reqwest).

use std::sync::Arc;

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, ORIGIN, REFERER as H_REFERER, USER_AGENT as H_UA};
use serde_json::json;

use crate::config::Config;
use crate::constants::{ALLANIME_API, EPISODE_QUERY_HASH, REFERER, USER_AGENT};
use crate::minter::{
    self, bucket_ts, sign_aa_req, Capabilities, Material, MaterialCache, MaterialProvider,
    PersistedContext, ALLANIME_SOURCE,
};
use crate::providers::{self, SourceUrl};

// GraphQL documents copied verbatim from the Python `api.py`.
const SEARCH_GQL: &str = "query( $search: SearchInput $limit: Int $page: Int \
    $translationType: VaildTranslationTypeEnumType \
    $countryOrigin: VaildCountryOriginEnumType ) { \
    shows( search: $search limit: $limit page: $page \
    translationType: $translationType countryOrigin: $countryOrigin ) { \
    edges { _id name availableEpisodes airedStart __typename } } }";

const EPISODES_LIST_GQL: &str = "query ($showId: String!) { show( _id: $showId ) \
    { _id availableEpisodesDetail } }";

const EPISODE_EMBED_GQL: &str = "query ($showId: String!, \
    $translationType: VaildTranslationTypeEnumType!, $episodeString: String!) { \
    episode( showId: $showId translationType: $translationType \
    episodeString: $episodeString ) { episodeString sourceUrls } }";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslationType {
    Sub,
    Dub,
}

impl TranslationType {
    pub fn as_str(&self) -> &'static str {
        match self {
            TranslationType::Sub => "sub",
            TranslationType::Dub => "dub",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ShowResult {
    pub id: String,
    pub name: String,
    pub episodes: u32,
    pub year: u32, // airedStart.year; 0 == unknown
}

pub struct AllAnimeClient {
    pub client: reqwest::Client,
    minter: Option<(Arc<dyn MaterialProvider>, Capabilities)>,
    material_cache: MaterialCache,
    /// Shared client for the current material referer (rebuilt only when the
    /// referer changes).
    referer_client: tokio::sync::Mutex<Option<(String, reqwest::Client)>>,
}

fn default_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(H_UA, HeaderValue::from_static(USER_AGENT));
    headers.insert(H_REFERER, HeaderValue::from_static(REFERER));
    headers.insert(ORIGIN, HeaderValue::from_static(REFERER));
    headers
}

fn headers_for(referer: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(H_UA, HeaderValue::from_static(USER_AGENT));
    headers.insert(
        H_REFERER,
        HeaderValue::from_str(referer).unwrap_or_else(|_| HeaderValue::from_static(REFERER)),
    );
    headers.insert(
        ORIGIN,
        HeaderValue::from_str(referer).unwrap_or_else(|_| HeaderValue::from_static(REFERER)),
    );
    headers
}

/// Client for video downloads. Unlike the API client, this must NOT set a
/// total request timeout — reqwest's `timeout` covers the entire body read,
/// which kills any download longer than the limit ("error decoding response
/// body"). Downloads instead get a connect timeout and a per-read stall
/// timeout, so a hung connection still errors out but a slow multi-minute
/// download does not.
pub fn download_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .default_headers(default_headers())
        .connect_timeout(std::time::Duration::from_secs(15))
        .read_timeout(std::time::Duration::from_secs(30))
        .build()
        .context("building download client")
}

impl AllAnimeClient {
    pub async fn new(cfg: &Config) -> Result<Self> {
        let client = reqwest::Client::builder()
            .default_headers(default_headers())
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .context("building reqwest client")?;

        let minter = if cfg.minter.is_enabled() {
            match minter::from_config(&cfg.minter).await {
                Ok(pair) => Some(pair),
                Err(e) => {
                    eprintln!(
                        "  ! minter init failed ({e:#}); falling back to legacy API path"
                    );
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            client,
            minter,
            material_cache: MaterialCache::default(),
            referer_client: tokio::sync::Mutex::new(None),
        })
    }

    /// `(api_base, client)` for GraphQL calls: minter material when cached,
    /// otherwise the hardcoded legacy endpoint.
    async fn api_context(&self) -> Result<(String, reqwest::Client)> {
        if self.minter.is_some() {
            if let Some(material) = self.material_cache.get(ALLANIME_SOURCE).await {
                let client = self.client_for(&material.referer).await?;
                return Ok((
                    material.api_base.trim_end_matches('/').to_string(),
                    client,
                ));
            }
        }
        Ok((ALLANIME_API.to_string(), self.client.clone()))
    }

    async fn client_for(&self, referer: &str) -> Result<reqwest::Client> {
        let mut guard = self.referer_client.lock().await;
        if let Some((cached_referer, client)) = guard.as_ref() {
            if cached_referer == referer {
                return Ok(client.clone());
            }
        }
        let client = reqwest::Client::builder()
            .default_headers(headers_for(referer))
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .context("building referer client")?;
        *guard = Some((referer.to_string(), client.clone()));
        Ok(client)
    }

    pub async fn search(
        &self,
        query: &str,
        mode: TranslationType,
    ) -> Result<Vec<ShowResult>> {
        let query = query
            .replace(['\'', '\u{2018}', '\u{2019}', '`'], " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let variables = json!({
            "search": { "allowAdult": false, "allowUnknown": false, "query": query },
            "limit": 40,
            "page": 1,
            "translationType": mode.as_str(),
            "countryOrigin": "ALL",
        });
        let body = json!({ "variables": variables, "query": SEARCH_GQL });
        let (api_base, client) = self.api_context().await?;
        let resp = client
            .post(format!("{api_base}/api"))
            .json(&body)
            .send()
            .await
            .context("search request failed")?;
        let v: serde_json::Value = resp.json().await.context("search: bad JSON")?;

        let mut out = Vec::new();
        if let Some(edges) = v
            .get("data")
            .and_then(|d| d.get("shows"))
            .and_then(|s| s.get("edges"))
            .and_then(|e| e.as_array())
        {
            for e in edges {
                let count = e
                    .get("availableEpisodes")
                    .and_then(|a| a.get(mode.as_str()))
                    .and_then(|n| n.as_u64())
                    .unwrap_or(0) as u32;
                if count == 0 {
                    continue;
                }
                let year = e
                    .get("airedStart")
                    .and_then(|a| a.get("year"))
                    .and_then(|y| y.as_u64())
                    .unwrap_or(0) as u32;
                out.push(ShowResult {
                    id: e.get("_id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    name: e.get("name").and_then(|x| x.as_str()).unwrap_or("?").to_string(),
                    episodes: count,
                    year,
                });
            }
        }
        Ok(out)
    }

    pub async fn episode_list(
        &self,
        show_id: &str,
        mode: TranslationType,
    ) -> Result<Vec<String>> {
        let body = json!({
            "variables": { "showId": show_id },
            "query": EPISODES_LIST_GQL,
        });
        let (api_base, client) = self.api_context().await?;
        let resp = client
            .post(format!("{api_base}/api"))
            .json(&body)
            .send()
            .await
            .context("episode_list request failed")?;
        let v: serde_json::Value = resp.json().await.context("episode_list: bad JSON")?;

        let mut eps: Vec<String> = v
            .get("data")
            .and_then(|d| d.get("show"))
            .and_then(|s| s.get("availableEpisodesDetail"))
            .and_then(|d| d.get(mode.as_str()))
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        eps.sort_by(|a, b| {
            let fa: f64 = a.parse().unwrap_or(0.0);
            let fb: f64 = b.parse().unwrap_or(0.0);
            fa.partial_cmp(&fb).unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(eps)
    }

    pub async fn episode_sources(
        &self,
        show_id: &str,
        ep: &str,
        mode: TranslationType,
    ) -> Result<Vec<SourceUrl>> {
        // Primary path: ani-cli's current recipe — a persisted-query GET carrying
        // the youtu-chan referer/origin and NO aaReq. AllAnime leaves this path
        // open and still encrypts the reply with the static `tobeparsed` key, so
        // it needs neither the browser-gated crypto bootstrap nor the minter.
        match self.episode_sources_direct(show_id, ep, mode).await {
            Ok(sources) if !sources.is_empty() => return Ok(sources),
            Ok(_) => {}
            Err(e) => eprintln!("  ! direct API path failed: {e:#}"),
        }

        // Fallback: the aaReq minter path, only when one is configured. Kept for
        // the day AllAnime closes the direct path again; harmless otherwise.
        if let Some((minter, caps)) = &self.minter {
            return self
                .episode_sources_minter(minter.as_ref(), caps, show_id, ep, mode)
                .await;
        }

        Ok(Vec::new())
    }

    async fn episode_sources_minter(
        &self,
        minter: &dyn MaterialProvider,
        caps: &Capabilities,
        show_id: &str,
        ep: &str,
        mode: TranslationType,
    ) -> Result<Vec<SourceUrl>> {
        if caps.supports_level(0) {
            match self
                .episode_sources_level0(minter, show_id, ep, mode, false)
                .await
            {
                Ok(sources) if !sources.is_empty() => return Ok(sources),
                Ok(_) => {}
                Err(e) if is_crypto_rejection(&format!("{e:#}")) => {}
                Err(e) => return Err(e),
            }

            self.material_cache.invalidate(ALLANIME_SOURCE).await;
            if let Ok(sources) = self
                .episode_sources_level0(minter, show_id, ep, mode, true)
                .await
            {
                if !sources.is_empty() {
                    return Ok(sources);
                }
            }
        }

        if caps.supports_level(1) {
            if let Ok(sources) = self
                .episode_sources_level1(minter, show_id, ep, mode)
                .await
            {
                if !sources.is_empty() {
                    return Ok(sources);
                }
            }
        }

        if caps.supports_level(2) {
            return minter.resolve(ALLANIME_SOURCE, show_id, ep, mode).await;
        }

        // The direct path already ran (and failed) before we got here, so there
        // is nothing left to try.
        eprintln!("  ! minter enabled but all levels failed");
        Ok(Vec::new())
    }

    async fn episode_sources_level0(
        &self,
        minter: &dyn MaterialProvider,
        show_id: &str,
        ep: &str,
        mode: TranslationType,
        force: bool,
    ) -> Result<Vec<SourceUrl>> {
        let material = self.get_material(minter, force).await?;
        let ts = bucket_ts(chrono::Utc::now().timestamp_millis());
        let aa_req = sign_aa_req(
            &material.part_b,
            &material.mask,
            material.epoch,
            &material.build_id,
            EPISODE_QUERY_HASH,
            ts,
        )?;
        let text = self
            .fetch_episode_with_aa_req(&material, show_id, ep, mode, &aa_req)
            .await?;
        if is_crypto_rejection(&text) {
            self.material_cache.invalidate(ALLANIME_SOURCE).await;
            anyhow::bail!("AA_CRYPTO rejection");
        }
        Ok(providers::parse_source_urls(&text))
    }

    async fn episode_sources_level1(
        &self,
        minter: &dyn MaterialProvider,
        show_id: &str,
        ep: &str,
        mode: TranslationType,
    ) -> Result<Vec<SourceUrl>> {
        let ts = bucket_ts(chrono::Utc::now().timestamp_millis());
        let token = minter
            .sign(ALLANIME_SOURCE, EPISODE_QUERY_HASH, Some(ts))
            .await?;
        let material = self.get_material(minter, false).await?;
        let text = self
            .fetch_episode_with_aa_req(&material, show_id, ep, mode, &token.aa_req)
            .await?;
        if is_crypto_rejection(&text) {
            self.material_cache.invalidate(ALLANIME_SOURCE).await;
            anyhow::bail!("AA_CRYPTO rejection");
        }
        Ok(providers::parse_source_urls(&text))
    }

    async fn get_material(
        &self,
        minter: &dyn MaterialProvider,
        force: bool,
    ) -> Result<Material> {
        if !force {
            if let Some(cached) = self.material_cache.get(ALLANIME_SOURCE).await {
                return Ok(cached);
            }
        }
        match minter.material(ALLANIME_SOURCE, force).await {
            Ok(material) => {
                self.record_material(&material)?;
                self.material_cache
                    .set(ALLANIME_SOURCE, material.clone())
                    .await;
                Ok(material)
            }
            Err(e) if !force => {
                if let Some(cached) = self.material_cache.get(ALLANIME_SOURCE).await {
                    return Ok(cached);
                }
                if minter::load_persisted_context(ALLANIME_SOURCE).is_some() {
                    eprintln!(
                        "  ! minter unavailable; stale material context on disk but rotating secrets expired"
                    );
                }
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    fn record_material(&self, material: &Material) -> Result<()> {
        let ctx = PersistedContext {
            mask: material.mask.clone(),
            build_id: material.build_id.clone(),
            referer: material.referer.clone(),
            api_base: material.api_base.clone(),
            cdn_base: material.cdn_base.clone(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };
        minter::save_persisted_context(ALLANIME_SOURCE, &ctx)
    }

    async fn fetch_episode_with_aa_req(
        &self,
        material: &Material,
        show_id: &str,
        ep: &str,
        mode: TranslationType,
        aa_req: &str,
    ) -> Result<String> {
        let variables = json!({
            "showId": show_id,
            "translationType": mode.as_str(),
            "episodeString": ep,
        });
        let extensions = json!({
            "persistedQuery": { "version": 1, "sha256Hash": EPISODE_QUERY_HASH },
            "aaReq": aa_req,
        });

        let client = self.client_for(&material.referer).await?;

        let body = json!({ "variables": variables, "extensions": extensions });
        let resp = client
            .post(format!("{}/api", material.api_base.trim_end_matches('/')))
            .json(&body)
            .send()
            .await
            .context("episode_sources POST (aaReq) failed")?;
        let mut text = resp.text().await.context("episode_sources: reading body")?;

        if !text.contains("tobeparsed") && !text.contains("sourceUrl") {
            let body = json!({
                "variables": variables,
                "extensions": extensions,
                "query": EPISODE_EMBED_GQL,
            });
            let resp = client
                .post(format!("{}/api", material.api_base.trim_end_matches('/')))
                .json(&body)
                .send()
                .await
                .context("episode_sources POST (full query) failed")?;
            text = resp
                .text()
                .await
                .context("episode_sources: reading POST body")?;
        }
        Ok(text)
    }

    /// The no-aaReq persisted-query GET path (ani-cli's current recipe). This is
    /// now the primary resolver; the minter is only a fallback.
    async fn episode_sources_direct(
        &self,
        show_id: &str,
        ep: &str,
        mode: TranslationType,
    ) -> Result<Vec<SourceUrl>> {
        let variables = json!({
            "showId": show_id,
            "translationType": mode.as_str(),
            "episodeString": ep,
        });
        let extensions = json!({
            "persistedQuery": { "version": 1, "sha256Hash": EPISODE_QUERY_HASH }
        });

        // Pin to the hardcoded endpoint + youtu-chan header client. This path is
        // the verified recipe and must not inherit minter material, which may
        // carry a different referer/api_base.
        let api_base = ALLANIME_API.to_string();
        let client = self.client.clone();
        let mut text = String::new();
        if let Ok(resp) = client
            .get(format!("{api_base}/api"))
            .query(&[
                ("variables", variables.to_string()),
                ("extensions", extensions.to_string()),
            ])
            .send()
            .await
        {
            text = resp.text().await.unwrap_or_default();
        }

        if !text.contains("tobeparsed") && !text.contains("sourceUrl") {
            let body = json!({ "variables": variables, "query": EPISODE_EMBED_GQL });
            let resp = client
                .post(format!("{api_base}/api"))
                .json(&body)
                .send()
                .await
                .context("episode_sources POST failed")?;
            text = resp
                .text()
                .await
                .context("episode_sources: reading POST body")?;
        }

        Ok(providers::parse_source_urls(&text))
    }
}

fn is_crypto_rejection(text: &str) -> bool {
    text.contains("AA_CRYPTO")
}
